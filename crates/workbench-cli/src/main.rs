use std::collections::{BTreeMap, HashSet};
use std::env;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use anyhow::{Context, Result, anyhow, bail};
use clap::{ArgAction, Parser, Subcommand};
use tracing_subscriber::EnvFilter;
use workbench_core::{
    CandidateDecision, ObservedState, Recipe, ReusePolicy, WindowSpec, build_reconcile_plan,
    choose_candidate, load_config,
};
use workbench_niri::{NiriSession, format_action, reconcile};

#[derive(Parser, Debug)]
#[command(
    name = "niri-workbench",
    version,
    about = "Reproducible project workspaces for Niri"
)]
struct Cli {
    /// Path to config.toml.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Override NIRI_SOCKET.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,

    /// Increase diagnostic logging (-v, -vv).
    #[arg(short = 'v', action = ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// List configured workbenches.
    List,
    /// Show one workbench recipe.
    Show { name: String },
    /// Compare a recipe with the current Niri state.
    Status { name: String },
    /// Show what apply would do without changing anything.
    Plan { name: String },
    /// Open a workbench: reuse existing apps, launch missing ones, and restore layout.
    Open { name: String },
    /// Reconcile the current Niri state with a recipe.
    Apply {
        name: String,
        /// Print the plan without spawning or moving anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Restore layout using existing windows only; never launch missing applications.
    Repair { name: String },
    /// Open the full graphical Workbench manager.
    Ui,
    /// Open the compact graphical workbench launcher.
    Quick,
    /// Check Niri, IPC, config, commands, outputs, and recipe consistency.
    Doctor,
}

#[derive(Debug)]
enum ExistingResolution {
    Reuse(u64),
    Spawn,
}

#[derive(Debug)]
struct ResolvedExisting {
    by_name: BTreeMap<String, ExistingResolution>,
    ids: BTreeMap<String, u64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    let config_path = cli.config.clone().unwrap_or_else(default_config_path);

    match cli.command {
        None => launch_ui(true),
        Some(Commands::Ui) => launch_ui(false),
        Some(Commands::Quick) => launch_ui(true),
        Some(Commands::Doctor) => doctor(&config_path, cli.socket.as_deref()).await,
        Some(Commands::List) => {
            let config = load_config(&config_path)?;
            for (key, recipe) in config.workbench {
                let display = recipe.name.as_deref().unwrap_or(&recipe.workspace);
                println!("{key:<20} {display}  [workspace: {}]", recipe.workspace);
            }
            Ok(())
        }
        Some(Commands::Show { name }) => {
            let config = load_config(&config_path)?;
            let recipe = recipe(&config.workbench, &name)?;
            print_recipe(&name, recipe);
            Ok(())
        }
        Some(Commands::Status { name }) => {
            let config = load_config(&config_path)?;
            let recipe = recipe(&config.workbench, &name)?;
            let session = connect(cli.socket.as_deref()).await?;
            status(&name, recipe, &session).await
        }
        Some(Commands::Plan { name }) => {
            let config = load_config(&config_path)?;
            let recipe = recipe(&config.workbench, &name)?;
            let session = connect(cli.socket.as_deref()).await?;
            plan(&name, recipe, &session).await
        }
        Some(Commands::Open { name }) => {
            let config = load_config(&config_path)?;
            let recipe = recipe(&config.workbench, &name)?;
            let session = connect(cli.socket.as_deref()).await?;
            apply(&name, recipe, &session).await
        }
        Some(Commands::Apply { name, dry_run }) => {
            let config = load_config(&config_path)?;
            let recipe = recipe(&config.workbench, &name)?;
            let session = connect(cli.socket.as_deref()).await?;
            if dry_run {
                plan(&name, recipe, &session).await
            } else {
                apply(&name, recipe, &session).await
            }
        }
        Some(Commands::Repair { name }) => {
            let config = load_config(&config_path)?;
            let recipe = recipe(&config.workbench, &name)?;
            let session = connect(cli.socket.as_deref()).await?;
            repair(&name, recipe, &session).await
        }
    }
}

fn launch_ui(quick: bool) -> Result<()> {
    let current = env::current_exe().context("could not locate niri-workbench executable")?;
    let sibling = current.with_file_name("niri-workbench-ui");
    let program = if sibling.exists() {
        sibling
    } else {
        PathBuf::from("niri-workbench-ui")
    };

    let mut command = ProcessCommand::new(&program);
    if quick {
        command.arg("--quick");
    }
    command.spawn().with_context(|| {
        format!(
            "could not launch {}; install the UI binary with scripts/install.sh",
            program.display()
        )
    })?;
    Ok(())
}

fn init_tracing(verbose: u8) {
    let fallback = match verbose {
        0 => "niri_workbench=warn,workbench_niri=warn",
        1 => "niri_workbench=info,workbench_niri=info",
        _ => "niri_workbench=debug,workbench_niri=debug",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(fallback));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(verbose > 1)
        .without_time()
        .compact()
        .init();
}

fn default_config_path() -> PathBuf {
    if let Some(config_home) = env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(config_home)
            .join("niri-workbench")
            .join("config.toml");
    }
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("niri-workbench")
        .join("config.toml")
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

async fn connect(socket: Option<&Path>) -> Result<NiriSession> {
    let socket = match socket {
        Some(socket) => socket.to_path_buf(),
        None => NiriSession::default_socket_path()?,
    };
    NiriSession::connect(socket)
        .await
        .context("could not initialize Niri IPC")
}

fn recipe<'a>(workbenches: &'a BTreeMap<String, Recipe>, name: &str) -> Result<&'a Recipe> {
    workbenches.get(name).ok_or_else(|| {
        let choices = workbenches.keys().cloned().collect::<Vec<_>>().join(", ");
        anyhow!("unknown workbench {name:?}; configured: {choices}")
    })
}

fn print_recipe(name: &str, recipe: &Recipe) {
    println!("Workbench: {name}");
    if let Some(display) = &recipe.name {
        println!("Name:      {display}");
    }
    println!("Workspace: {}", recipe.workspace);
    if let Some(output) = &recipe.output {
        println!("Output:    {output}");
    }
    if let Some(focus) = &recipe.focus {
        println!("Focus:     {focus}");
    }
    println!();
    for window in &recipe.windows {
        println!("{}:", window.name);
        println!("  command: {}", shellish(&window.command));
        println!("  match:   {}", matcher_text(window));
        println!("  column:  {}", window.layout.column);
        if let Some(width) = window.layout.column_width {
            println!("  width:   {width}");
        }
        if let Some(height) = window.layout.window_height {
            println!("  height:  {height}");
        }
        if let Some(display) = window.layout.display {
            println!("  display: {display:?}");
        }
        if window.layout.floating {
            println!("  mode:    floating");
        }
    }
}

async fn status(name: &str, recipe: &Recipe, session: &NiriSession) -> Result<()> {
    let target = session.resolve_target_output(recipe).await?;
    if let Some(warning) = &target.fallback_warning {
        eprintln!("warning: {warning}");
    }

    let snapshot = session.snapshot().await;
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.name.as_deref() == Some(recipe.workspace.as_str()));

    println!("Status: {name}");
    match workspace {
        Some(workspace) => {
            println!(
                "workspace {}: present (id {}, output {})",
                recipe.workspace,
                workspace.id,
                workspace.output.as_deref().unwrap_or("?")
            );
            if workspace.output.as_deref() != Some(target.name.as_str()) {
                println!(
                    "DRIFT   workspace output {:?} -> {:?}",
                    workspace.output, target.name
                );
            }
        }
        None => println!("MISSING workspace {:?}", recipe.workspace),
    }

    let resolved = resolve_existing(recipe, &snapshot)?;
    print_resolution(recipe, &resolved);

    if let Some(workspace) = workspace {
        if resolved.ids.len() == recipe.windows.len() {
            let plan = build_reconcile_plan(recipe, &snapshot, &resolved.ids, workspace.id, false);
            if plan.actions.is_empty() {
                println!("observable layout: converged");
            } else {
                println!("observable layout: drift");
                for action in &plan.actions {
                    println!("  {}", format_action(action));
                }
            }
        }
    }

    if recipe
        .windows
        .iter()
        .any(|window| window.layout.display.is_some())
    {
        println!(
            "note: Niri 26.04 does not expose column display mode; tabbed/normal cannot be verified"
        );
    }
    Ok(())
}

async fn plan(name: &str, recipe: &Recipe, session: &NiriSession) -> Result<()> {
    let target = session.resolve_target_output(recipe).await?;
    println!("Plan: {name}");
    if let Some(warning) = target.fallback_warning {
        println!("WARN    {warning}");
    }

    let snapshot = session.snapshot().await;
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.name.as_deref() == Some(recipe.workspace.as_str()));

    match workspace {
        Some(workspace) if workspace.output.as_deref() == Some(target.name.as_str()) => {
            println!(
                "KEEP    workspace {:?} on {}",
                recipe.workspace, target.name
            );
        }
        Some(workspace) => {
            println!(
                "MOVE    workspace {:?} {:?} -> {}",
                recipe.workspace, workspace.output, target.name
            );
        }
        None => {
            println!(
                "CREATE  named workspace {:?} on {} using an empty Niri workspace",
                recipe.workspace, target.name
            );
        }
    }

    let resolved = resolve_existing(recipe, &snapshot)?;
    for spec in &recipe.windows {
        match resolved.by_name.get(&spec.name) {
            Some(ExistingResolution::Reuse(id)) => {
                println!("REUSE   {:<12} window {id}", spec.name)
            }
            Some(ExistingResolution::Spawn) => {
                println!("SPAWN   {:<12} {}", spec.name, shellish(&spec.command))
            }
            None => {}
        }
    }

    if let Some(workspace) = workspace {
        if resolved.ids.len() == recipe.windows.len() {
            let plan = build_reconcile_plan(recipe, &snapshot, &resolved.ids, workspace.id, false);
            for action in &plan.actions {
                println!("{}", format_action(action));
            }
        } else {
            print_desired_layout(recipe);
        }
    } else {
        print_desired_layout(recipe);
    }

    for (column, display) in declared_displays(recipe) {
        println!(
            "DISPLAY column {column} -> {display} (reasserted; current mode is not observable)"
        );
    }
    if let Some(focus) = &recipe.focus {
        println!("FOCUS   {focus}");
    }
    Ok(())
}

async fn apply(name: &str, recipe: &Recipe, session: &NiriSession) -> Result<()> {
    let target = session.resolve_target_output(recipe).await?;
    if let Some(warning) = &target.fallback_warning {
        eprintln!("warning: {warning}");
    }

    let initial = session.snapshot().await;
    let existing = resolve_existing(recipe, &initial)?;
    let workspace_id = session
        .ensure_workspace(&recipe.workspace, &target.name)
        .await?;

    let mut ids = existing.ids;
    let mut used: HashSet<u64> = ids.values().copied().collect();
    let mut activity = Vec::new();

    for spec in &recipe.windows {
        match existing.by_name.get(&spec.name) {
            Some(ExistingResolution::Reuse(id)) => {
                activity.push(format!("reuse {} window {id}", spec.name));
            }
            Some(ExistingResolution::Spawn) => {
                let id = session
                    .spawn_and_wait(spec, &used, recipe.spawn_timeout_ms)
                    .await
                    .with_context(|| format!("while spawning logical window {:?}", spec.name))?;
                used.insert(id);
                ids.insert(spec.name.clone(), id);
                activity.push(format!("spawn {} -> window {id}", spec.name));
            }
            None => bail!("internal error: no resolution for {:?}", spec.name),
        }
    }

    let report = reconcile(session, recipe, &ids, workspace_id).await?;
    activity.extend(report.executed.iter().map(format_action));

    if !report.remaining.is_empty() {
        eprintln!("warning: observable state still has drift:");
        for action in &report.remaining {
            eprintln!("  {}", format_action(action));
        }
    }
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }

    let observable_mutations = report
        .executed
        .iter()
        .filter(|action| !action.display_is_unverifiable())
        .count();
    let spawned = existing
        .by_name
        .values()
        .filter(|resolution| matches!(resolution, ExistingResolution::Spawn))
        .count();

    if spawned == 0
        && observable_mutations == 0
        && report.remaining.is_empty()
        && target.fallback_warning.is_none()
    {
        if report.display_reassertions == 0 {
            println!("{name} already converged");
        } else {
            println!(
                "{name} observable state already converged; reasserted {} unobservable column display mode(s)",
                report.display_reassertions
            );
        }
    } else {
        println!("Applied {name}:");
        for line in activity {
            println!("  {line}");
        }
        if report.display_reassertions > 0 {
            println!(
                "  note: {} column display mode(s) were reasserted because Niri does not expose them",
                report.display_reassertions
            );
        }
    }

    Ok(())
}

async fn repair(name: &str, recipe: &Recipe, session: &NiriSession) -> Result<()> {
    let target = session.resolve_target_output(recipe).await?;
    if let Some(warning) = &target.fallback_warning {
        eprintln!("warning: {warning}");
    }

    let snapshot = session.snapshot().await;
    let mut used = HashSet::new();
    let mut ids = BTreeMap::new();
    let mut missing = Vec::new();

    for spec in &recipe.windows {
        match choose_candidate(spec, &snapshot.windows, &used, None)
            .with_context(|| format!("invalid matcher for logical window {:?}", spec.name))?
        {
            CandidateDecision::None => missing.push(spec.name.clone()),
            CandidateDecision::One(candidate) => {
                used.insert(candidate.id);
                ids.insert(spec.name.clone(), candidate.id);
            }
            CandidateDecision::Ambiguous(candidates) => {
                bail!(
                    "cannot repair {:?}: {} existing windows match; refine its matcher first",
                    spec.name,
                    candidates.len()
                );
            }
        }
    }

    if !missing.is_empty() {
        bail!(
            "repair never launches applications; missing window(s): {}. Use niri-workbench open {name} instead.",
            missing.join(", ")
        );
    }

    let workspace_id = session
        .ensure_workspace(&recipe.workspace, &target.name)
        .await?;
    let report = reconcile(session, recipe, &ids, workspace_id).await?;

    if report.executed.is_empty() && report.remaining.is_empty() {
        println!("{name} observable layout already converged");
    } else {
        println!("Repaired {name}:");
        for action in &report.executed {
            println!("  {}", format_action(action));
        }
    }
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    if !report.remaining.is_empty() {
        eprintln!("warning: observable state still has drift:");
        for action in &report.remaining {
            eprintln!("  {}", format_action(action));
        }
    }
    Ok(())
}

fn resolve_existing(recipe: &Recipe, state: &ObservedState) -> Result<ResolvedExisting> {
    let mut used = HashSet::new();
    let mut by_name = BTreeMap::new();
    let mut ids = BTreeMap::new();

    for spec in &recipe.windows {
        if spec.reuse == ReusePolicy::Never {
            by_name.insert(spec.name.clone(), ExistingResolution::Spawn);
            continue;
        }

        match choose_candidate(spec, &state.windows, &used, None)
            .with_context(|| format!("invalid matcher for logical window {:?}", spec.name))?
        {
            CandidateDecision::None => {
                if spec.command.is_empty() {
                    bail!(
                        "could not resolve window {:?}: no matching window exists and command is empty",
                        spec.name
                    );
                }
                by_name.insert(spec.name.clone(), ExistingResolution::Spawn);
            }
            CandidateDecision::One(candidate) => {
                used.insert(candidate.id);
                ids.insert(spec.name.clone(), candidate.id);
                by_name.insert(spec.name.clone(), ExistingResolution::Reuse(candidate.id));
            }
            CandidateDecision::Ambiguous(candidates) => {
                let mut message = String::new();
                writeln!(
                    message,
                    "could not safely resolve window {:?}: {} pre-existing windows match",
                    spec.name,
                    candidates.len()
                )?;
                writeln!(message, "matcher: {}", matcher_text(spec))?;
                writeln!(message, "candidates:")?;
                for candidate in candidates {
                    writeln!(
                        message,
                        "  id={} app_id={:?} title={:?} pid={:?} process={:?}",
                        candidate.id,
                        candidate.app_id,
                        candidate.title,
                        candidate.pid,
                        candidate.process_exe
                    )?;
                }
                writeln!(
                    message,
                    "refine the matcher (usually title/process), or set reuse = \"never\" if a new window is intentional"
                )?;
                bail!(message.trim_end().to_owned());
            }
        }
    }

    Ok(ResolvedExisting { by_name, ids })
}

fn print_resolution(recipe: &Recipe, resolved: &ResolvedExisting) {
    for spec in &recipe.windows {
        match resolved.by_name.get(&spec.name) {
            Some(ExistingResolution::Reuse(id)) => {
                println!("FOUND   {:<12} window {id}", spec.name);
            }
            Some(ExistingResolution::Spawn) => {
                println!(
                    "MISSING {:<12} would spawn {}",
                    spec.name,
                    shellish(&spec.command)
                );
            }
            None => {}
        }
    }
}

fn print_desired_layout(recipe: &Recipe) {
    for spec in &recipe.windows {
        if spec.layout.floating {
            println!("PLACE   {:<12} floating", spec.name);
            continue;
        }
        let mut detail = format!("column {}", spec.layout.column);
        if let Some(width) = spec.layout.column_width {
            write!(detail, ", width {width}").ok();
        }
        if let Some(height) = spec.layout.window_height {
            write!(detail, ", height {height}").ok();
        }
        println!("PLACE   {:<12} {detail}", spec.name);
    }
}

fn declared_displays(recipe: &Recipe) -> BTreeMap<usize, &'static str> {
    let mut displays = BTreeMap::new();
    for spec in &recipe.windows {
        if let Some(display) = spec.layout.display {
            displays.insert(
                spec.layout.column,
                match display {
                    workbench_core::ColumnDisplay::Normal => "normal",
                    workbench_core::ColumnDisplay::Tabbed => "tabbed",
                },
            );
        }
    }
    displays
}

fn matcher_text(spec: &WindowSpec) -> String {
    let mut parts = Vec::new();
    if let Some(value) = spec.match_spec.window_id {
        parts.push(format!("window_id={value}"));
    }
    if let Some(value) = &spec.match_spec.app_id {
        parts.push(format!("app_id={value:?}"));
    }
    if let Some(value) = &spec.match_spec.title {
        parts.push(format!("title={value:?}"));
    }
    if let Some(value) = &spec.match_spec.process {
        parts.push(format!("process={value:?}"));
    }
    if let Some(value) = spec.match_spec.pid {
        parts.push(format!("pid={value}"));
    }
    parts.join(", ")
}

fn shellish(command: &[String]) -> String {
    if command.is_empty() {
        return "(no command)".to_owned();
    }
    command
        .iter()
        .map(|arg| {
            if arg
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || "/._-:=+%@".contains(ch))
            {
                arg.clone()
            } else {
                format!("{arg:?}")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

async fn doctor(config_path: &Path, socket_override: Option<&Path>) -> Result<()> {
    let mut errors = 0usize;
    let mut warnings = 0usize;

    let niri_path = resolve_executable(OsStr::new("niri"));
    if let Some(path) = &niri_path {
        println!("✓ niri executable {}", path.display());
    } else {
        println!("✗ niri executable not found in PATH");
        errors += 1;
    }

    let config = match load_config(config_path) {
        Ok(config) => {
            println!("✓ config parsed {}", config_path.display());
            println!("✓ {} workbench(es)", config.workbench.len());
            Some(config)
        }
        Err(err) => {
            println!("✗ config: {err}");
            errors += 1;
            None
        }
    };

    let socket = socket_override
        .map(PathBuf::from)
        .or_else(|| env::var_os("NIRI_SOCKET").map(PathBuf::from));

    let session = if let Some(socket) = socket {
        match NiriSession::connect(&socket).await {
            Ok(session) => {
                match session.version_string().await {
                    Ok(version) => {
                        println!("✓ Niri IPC connected ({version})");
                        if !version.starts_with("26.04") {
                            println!(
                                "! this release was validated against Niri 26.04 / niri-ipc 26.4.0"
                            );
                            warnings += 1;
                        }
                    }
                    Err(err) => {
                        println!("! IPC connected but version request failed: {err}");
                        warnings += 1;
                    }
                }
                Some(session)
            }
            Err(err) => {
                println!("✗ Niri IPC: {err}");
                errors += 1;
                None
            }
        }
    } else {
        println!("✗ NIRI_SOCKET is not set");
        errors += 1;
        None
    };

    if let Some(config) = &config {
        let mut resolved_commands = 0usize;
        for (recipe_name, recipe) in &config.workbench {
            for window in &recipe.windows {
                if window.command.is_empty() {
                    println!(
                        "! {recipe_name}.{} has no command; it can only reuse an existing window",
                        window.name
                    );
                    warnings += 1;
                    continue;
                }
                match expand_first_arg(&window.command[0]) {
                    Ok(binary) => {
                        if resolve_executable(OsStr::new(&binary)).is_some() {
                            resolved_commands += 1;
                        } else {
                            println!(
                                "✗ {recipe_name}.{} command not found: {binary}",
                                window.name
                            );
                            errors += 1;
                        }
                    }
                    Err(err) => {
                        println!(
                            "✗ {recipe_name}.{} command expansion failed: {err}",
                            window.name
                        );
                        errors += 1;
                    }
                }
            }
        }
        println!("✓ {resolved_commands} command(s) resolved");

        if let Some(session) = &session {
            let state = session.snapshot().await;
            let connected: HashSet<&str> = state
                .outputs
                .iter()
                .filter(|output| output.width.is_some())
                .map(|output| output.name.as_str())
                .collect();

            if connected.is_empty() {
                println!("! Niri reports no enabled outputs");
                warnings += 1;
            } else {
                println!(
                    "✓ output(s): {}",
                    connected.iter().copied().collect::<Vec<_>>().join(", ")
                );
            }

            for (name, recipe) in &config.workbench {
                if let Some(requested) = &recipe.output {
                    if !connected.contains(requested.as_str()) {
                        println!(
                            "! recipe {name:?}: output {requested:?} is currently disconnected"
                        );
                        warnings += 1;
                    }
                }
            }
        }
    }

    println!();
    println!("{errors} error(s), {warnings} warning(s)");
    if errors == 0 {
        Ok(())
    } else {
        bail!("doctor found {errors} error(s)")
    }
}

fn expand_first_arg(raw: &str) -> Result<String> {
    Ok(shellexpand::full(raw)
        .map_err(|err| anyhow!(err.to_string()))?
        .into_owned())
}

fn resolve_executable(command: &OsStr) -> Option<PathBuf> {
    let command_path = Path::new(command);
    if command_path.components().count() > 1 {
        return is_executable(command_path).then(|| command_path.to_path_buf());
    }

    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|directory| directory.join(command))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}
