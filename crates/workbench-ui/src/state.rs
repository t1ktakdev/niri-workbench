use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use niri_ipc::{Action as NiriAction, SizeChange};
use workbench_core::{
    Action, CandidateDecision, ColumnDisplay, Config, MatchSpec, ObservedState, OutputFallback,
    PlacementSpec, Recipe, ReusePolicy, RuntimeWindow, Size, WindowSpec, build_reconcile_plan,
    choose_candidate, load_config, save_config,
};
use workbench_niri::NiriSession;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipeStatus {
    Ready,
    Missing(usize),
    LayoutChanged,
    Ambiguous,
    Offline,
}

#[derive(Debug, Clone)]
pub struct CaptureDraft {
    pub recipe: Recipe,
    pub details: Vec<CapturedWindow>,
}

#[derive(Debug, Clone)]
pub struct CapturedWindow {
    pub title: String,
    pub app_id: String,
    pub cwd: Option<String>,
    pub include_by_default: bool,
    pub inclusion_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    pub desktop_id: String,
    pub name: String,
    pub icon: Option<String>,
    pub startup_wm_class: Option<String>,
    pub flatpak_id: Option<String>,
    pub command: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CliRunResult {
    pub success: bool,
    pub output: String,
}

pub fn load_config_or_empty(path: &Path) -> Result<Config> {
    if !path.exists() {
        return Ok(Config {
            workbench: BTreeMap::new(),
        });
    }
    load_config(path).map_err(Into::into)
}

pub fn persist_config(path: &Path, config: &Config) -> Result<()> {
    save_config(path, config).map_err(Into::into)
}

pub fn installed_applications() -> Vec<InstalledApp> {
    let mut seen = HashSet::new();
    let mut apps = Vec::new();

    for directory in desktop_application_dirs() {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("desktop") {
                continue;
            }
            let Some(desktop_id) = path
                .file_stem()
                .and_then(|value| value.to_str())
                .map(str::to_owned)
            else {
                continue;
            };
            if !seen.insert(desktop_id.clone()) {
                continue;
            }
            let Ok(contents) = fs::read_to_string(&path) else {
                continue;
            };
            if let Some(app) = parse_desktop_entry(&desktop_id, &contents) {
                apps.push(app);
            }
        }
    }

    apps.sort_by_key(|app| app.name.to_ascii_lowercase());
    apps
}

pub fn resolve_installed_app(app_id: &str) -> Option<InstalledApp> {
    resolve_installed_app_from(&installed_applications(), app_id).cloned()
}

fn resolve_installed_app_from<'a>(
    apps: &'a [InstalledApp],
    app_id: &str,
) -> Option<&'a InstalledApp> {
    let needle = app_id.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return None;
    }
    let normalized_needle = normalize_app_identity(&needle);

    apps.iter()
        .filter_map(|app| {
            let desktop = app.desktop_id.to_ascii_lowercase();
            let startup = app
                .startup_wm_class
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let flatpak = app
                .flatpak_id
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let executable = app
                .command
                .first()
                .and_then(|part| Path::new(part).file_name())
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();

            let score = if startup == needle {
                100
            } else if desktop == needle {
                95
            } else if flatpak == needle {
                90
            } else if executable == needle {
                85
            } else if normalize_app_identity(&desktop) == normalized_needle {
                80
            } else {
                0
            };
            (score > 0).then_some((score, app))
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, app)| app)
}

fn desktop_application_dirs() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(data_home) = env::var_os("XDG_DATA_HOME") {
        roots.push(PathBuf::from(data_home));
    } else if let Some(home) = env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".local/share"));
    }

    if let Some(data_dirs) = env::var_os("XDG_DATA_DIRS") {
        roots.extend(env::split_paths(&data_dirs));
    } else {
        roots.extend([
            PathBuf::from("/usr/local/share"),
            PathBuf::from("/usr/share"),
        ]);
    }

    let mut directories = Vec::new();
    for root in roots {
        directories.push(root.join("applications"));
        directories.push(root.join("flatpak/exports/share/applications"));
    }
    directories.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    directories
}

fn parse_desktop_entry(desktop_id: &str, contents: &str) -> Option<InstalledApp> {
    let mut in_desktop_entry = false;
    let mut values = HashMap::<String, String>::new();

    for raw in contents.lines() {
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        values
            .entry(key.to_owned())
            .or_insert_with(|| value.to_owned());
    }

    if values.get("Type").map(String::as_str) != Some("Application")
        || values.get("Hidden").is_some_and(|value| value == "true")
        || values.get("NoDisplay").is_some_and(|value| value == "true")
    {
        return None;
    }

    let name = values.get("Name")?.trim().to_owned();
    let raw_exec = values.get("Exec")?;
    let flatpak_id = values
        .get("X-Flatpak")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());

    let command = if let Some(flatpak_id) = flatpak_id.as_ref() {
        vec!["flatpak".to_owned(), "run".to_owned(), flatpak_id.clone()]
    } else {
        desktop_exec_command(raw_exec)?
    };

    Some(InstalledApp {
        desktop_id: desktop_id.to_owned(),
        name,
        icon: values
            .get("Icon")
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty()),
        startup_wm_class: values
            .get("StartupWMClass")
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty()),
        flatpak_id,
        command,
    })
}

fn desktop_exec_command(raw: &str) -> Option<Vec<String>> {
    let parts = shlex::split(raw)?;
    let command = parts
        .into_iter()
        .filter(|part| !part.starts_with("@@"))
        .filter(|part| !contains_desktop_field_code(part))
        .collect::<Vec<_>>();
    (!command.is_empty()).then_some(command)
}

fn contains_desktop_field_code(value: &str) -> bool {
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            continue;
        }
        let Some(next) = chars.next() else {
            return true;
        };
        if next != '%' {
            return true;
        }
    }
    false
}

fn normalize_app_identity(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

pub fn discover_socket() -> Result<PathBuf> {
    if let Some(socket) = env::var_os("NIRI_SOCKET") {
        return Ok(PathBuf::from(socket));
    }

    let runtime = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("NIRI_SOCKET and XDG_RUNTIME_DIR are not set"))?;

    let mut sockets = Vec::new();
    for entry in fs::read_dir(&runtime).with_context(|| format!("reading {}", runtime.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("niri.") && name.ends_with(".sock") {
            let modified = entry.metadata().and_then(|meta| meta.modified()).ok();
            sockets.push((modified, entry.path()));
        }
    }

    sockets.sort_by_key(|item| std::cmp::Reverse(item.0));
    sockets
        .into_iter()
        .next()
        .map(|(_, path)| path)
        .ok_or_else(|| anyhow!("could not find a Niri IPC socket in {}", runtime.display()))
}

pub fn niri_snapshot() -> Result<ObservedState> {
    let socket = discover_socket()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("creating Tokio runtime for Niri IPC")?;

    runtime.block_on(async move {
        let session = NiriSession::connect(socket).await?;
        Ok::<_, anyhow::Error>(session.snapshot().await)
    })
}

pub fn shape_own_window(width: i32, height: i32) {
    adjust_own_window(width, height, true);
}

pub fn ensure_own_window_size(width: i32, height: i32) {
    adjust_own_window(width, height, false);
}

fn adjust_own_window(width: i32, height: i32, force_floating_and_center: bool) {
    let Ok(socket) = discover_socket() else {
        return;
    };
    let Ok(pid) = i32::try_from(std::process::id()) else {
        return;
    };

    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => return,
        };

        runtime.block_on(async move {
            let Ok(session) = NiriSession::connect(socket).await else {
                return;
            };

            if session
                .wait_until(Duration::from_secs(2), |snapshot| {
                    snapshot
                        .windows
                        .iter()
                        .any(|window| window.pid == Some(pid))
                })
                .await
                .is_err()
            {
                return;
            }

            let snapshot = session.snapshot().await;
            let Some(window) = snapshot
                .windows
                .iter()
                .find(|window| window.pid == Some(pid))
                .cloned()
            else {
                return;
            };
            let window_id = window.id;

            if force_floating_and_center && !window.is_floating {
                if session
                    .send_action(NiriAction::MoveWindowToFloating {
                        id: Some(window_id),
                    })
                    .await
                    .is_err()
                {
                    return;
                }

                if session
                    .wait_until(Duration::from_secs(2), |snapshot| {
                        snapshot
                            .windows
                            .iter()
                            .find(|window| window.id == window_id)
                            .is_some_and(|window| window.is_floating)
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            } else if !window.is_floating {
                return;
            }

            if !force_floating_and_center {
                tokio::time::sleep(Duration::from_millis(120)).await;
            }

            let snapshot = session.snapshot().await;
            let Some(window) = snapshot
                .windows
                .iter()
                .find(|window| window.id == window_id)
            else {
                return;
            };
            if !window.is_floating {
                return;
            }

            let width_matches = (window.tile_width - f64::from(width)).abs() <= 1.0;
            let height_matches = (window.tile_height - f64::from(height)).abs() <= 1.0;

            if (force_floating_and_center || !width_matches)
                && session
                    .send_action(NiriAction::SetWindowWidth {
                        id: Some(window_id),
                        change: SizeChange::SetFixed(width),
                    })
                    .await
                    .is_err()
            {
                return;
            }

            if (force_floating_and_center || !height_matches)
                && session
                    .send_action(NiriAction::SetWindowHeight {
                        id: Some(window_id),
                        change: SizeChange::SetFixed(height),
                    })
                    .await
                    .is_err()
            {
                return;
            }

            if force_floating_and_center {
                let _ = session
                    .send_action(NiriAction::CenterWindow {
                        id: Some(window_id),
                    })
                    .await;
            }
        });
    });
}

pub fn recipe_status(recipe: &Recipe, state: Option<&ObservedState>) -> RecipeStatus {
    let Some(state) = state else {
        return RecipeStatus::Offline;
    };

    let mut used = HashSet::new();
    let mut ids = BTreeMap::new();
    let mut missing = 0usize;

    for spec in &recipe.windows {
        if spec.reuse == ReusePolicy::Never {
            missing += 1;
            continue;
        }

        match choose_candidate(spec, &state.windows, &used, None) {
            Ok(CandidateDecision::None) => missing += 1,
            Ok(CandidateDecision::One(candidate)) => {
                used.insert(candidate.id);
                ids.insert(spec.name.clone(), candidate.id);
            }
            Ok(CandidateDecision::Ambiguous(_)) | Err(_) => return RecipeStatus::Ambiguous,
        }
    }

    if missing > 0 {
        return RecipeStatus::Missing(missing);
    }

    let Some(workspace) = state
        .workspaces
        .iter()
        .find(|workspace| workspace.name.as_deref() == Some(recipe.workspace.as_str()))
    else {
        return RecipeStatus::LayoutChanged;
    };

    if ids.len() != recipe.windows.len() {
        return RecipeStatus::LayoutChanged;
    }

    let plan = build_reconcile_plan(recipe, state, &ids, workspace.id, false);
    if plan.actions.is_empty()
        || (workbench_ui_has_focus(state)
            && plan
                .actions
                .iter()
                .all(|action| matches!(action, Action::Focus { .. })))
    {
        RecipeStatus::Ready
    } else {
        RecipeStatus::LayoutChanged
    }
}

fn workbench_ui_has_focus(state: &ObservedState) -> bool {
    state.windows.iter().any(|window| {
        window.is_focused
            && window
                .app_id
                .as_deref()
                .unwrap_or_default()
                .starts_with("dev.t1ktak.NiriWorkbench")
    })
}

pub fn capture_workspace(
    state: &ObservedState,
    preferred_workspace_id: Option<u64>,
    preferred_focus_window_id: Option<u64>,
) -> Result<CaptureDraft> {
    let workspace = preferred_workspace_id
        .and_then(|id| state.workspaces.iter().find(|workspace| workspace.id == id))
        .or_else(|| state.focused_workspace())
        .ok_or_else(|| anyhow!("Niri has no workspace to capture"))?;

    let mut windows: Vec<&RuntimeWindow> = state
        .windows
        .iter()
        .filter(|window| window.workspace_id == Some(workspace.id))
        .filter(|window| {
            !window
                .app_id
                .as_deref()
                .unwrap_or_default()
                .starts_with("dev.t1ktak.NiriWorkbench")
        })
        .collect();

    if windows.is_empty() {
        bail!("focused workspace has no windows");
    }

    windows.sort_by_key(|window| {
        (
            window.column.unwrap_or(usize::MAX),
            window.tile_index.unwrap_or(usize::MAX),
            window.id,
        )
    });

    let mut column_counts = HashMap::<usize, usize>::new();
    for window in &windows {
        if let Some(column) = window.column {
            *column_counts.entry(column).or_default() += 1;
        }
    }

    // Matchers are evaluated against every Niri window, not only the captured workspace.
    // Count identities globally so Save current never creates a matcher that can steal an
    // unrelated window from another workspace.
    let mut app_id_counts = HashMap::<String, usize>::new();
    let mut app_cwd_counts = HashMap::<(String, String), usize>::new();
    for window in &state.windows {
        if window
            .app_id
            .as_deref()
            .unwrap_or_default()
            .starts_with("dev.t1ktak.NiriWorkbench")
        {
            continue;
        }
        if let Some(app_id) = window
            .app_id
            .as_ref()
            .filter(|value| !value.trim().is_empty())
        {
            *app_id_counts.entry(app_id.clone()).or_default() += 1;
            if let Some(cwd) = window.cwd.as_ref().filter(|value| !value.trim().is_empty()) {
                *app_cwd_counts
                    .entry((app_id.clone(), cwd.clone()))
                    .or_default() += 1;
            }
        }
    }

    let workspace_name = workspace
        .name
        .clone()
        .unwrap_or_else(|| format!("Workspace {}", workspace.index));
    let installed_apps = installed_applications();
    let project_root = infer_capture_project_root(&windows);
    let mut seen_names = HashSet::new();
    let mut seen_columns = HashSet::new();
    let mut specs = Vec::new();
    let mut details = Vec::new();
    let mut focus = None;

    for (index, window) in windows.iter().enumerate() {
        let logical = unique_window_name(window, index, &mut seen_names);
        if preferred_focus_window_id
            .map(|id| id == window.id)
            .unwrap_or(window.is_focused)
        {
            focus = Some(logical.clone());
        }

        let cwd = window.cwd.clone();
        let title = window.title.clone().unwrap_or_else(|| logical.clone());
        let app_id = window.app_id.clone().unwrap_or_default();

        let mut placement = PlacementSpec {
            column: window.column.unwrap_or(index + 1),
            floating: window.is_floating,
            ..PlacementSpec::default()
        };

        if !window.is_floating {
            if seen_columns.insert(placement.column) {
                placement.column_width = pixel_size(window.tile_width);
            }
            if column_counts.get(&placement.column).copied().unwrap_or(1) > 1 {
                placement.window_height = pixel_size(window.tile_height);
            }
        }

        let duplicate_app_id = window
            .app_id
            .as_ref()
            .and_then(|app_id| app_id_counts.get(app_id))
            .copied()
            .unwrap_or_default()
            > 1;
        let unique_cwd = if duplicate_app_id {
            window.app_id.as_ref().and_then(|app_id| {
                cwd.as_ref().and_then(|cwd| {
                    (app_cwd_counts
                        .get(&(app_id.clone(), cwd.clone()))
                        .copied()
                        .unwrap_or_default()
                        == 1)
                        .then(|| format!("^{}$", regex::escape(cwd)))
                })
            })
        } else {
            None
        };

        let match_spec = MatchSpec {
            window_id: None,
            app_id: window
                .app_id
                .as_ref()
                .map(|value| format!("^{}$", regex::escape(value))),
            title: capture_title_match(window, duplicate_app_id && unique_cwd.is_none()),
            process: None,
            cwd: unique_cwd,
            pid: None,
        };

        let (include_by_default, inclusion_note) =
            capture_inclusion(window, project_root.as_deref());
        specs.push(WindowSpec {
            name: logical,
            command: infer_command(window, cwd.as_deref(), &installed_apps),
            match_spec,
            reuse: ReusePolicy::Unique,
            layout: placement,
        });
        details.push(CapturedWindow {
            title,
            app_id,
            cwd,
            include_by_default,
            inclusion_note,
        });
    }

    Ok(CaptureDraft {
        recipe: Recipe {
            name: Some(workspace_name.clone()),
            workspace: workspace_name,
            output: workspace.output.clone(),
            output_fallback: OutputFallback::Focused,
            focus,
            spawn_timeout_ms: 10_000,
            windows: specs,
        },
        details,
    })
}

fn process_binary_name(window: &RuntimeWindow) -> Option<&str> {
    window
        .process_exe
        .as_deref()
        .and_then(|exe| Path::new(exe).file_name())
        .and_then(|name| name.to_str())
}

fn is_terminal_window(window: &RuntimeWindow) -> bool {
    let app = window
        .app_id
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let process = process_binary_name(window)
        .unwrap_or_default()
        .to_ascii_lowercase();
    [app.as_str(), process.as_str()].iter().any(|value| {
        value.contains("kitty")
            || value.contains("alacritty")
            || value.contains("wezterm")
            || *value == "foot"
            || value.contains("konsole")
            || value.contains("terminal")
    })
}

fn is_browser_window(window: &RuntimeWindow) -> bool {
    let app = window
        .app_id
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let process = process_binary_name(window)
        .unwrap_or_default()
        .to_ascii_lowercase();
    [app.as_str(), process.as_str()].iter().any(|value| {
        value.contains("chrome")
            || value.contains("chromium")
            || value.contains("firefox")
            || value.contains("brave")
            || value.contains("vivaldi")
    })
}

fn is_project_editor_window(window: &RuntimeWindow) -> bool {
    let app = window
        .app_id
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let process = process_binary_name(window)
        .unwrap_or_default()
        .to_ascii_lowercase();
    [app.as_str(), process.as_str()].iter().any(|value| {
        value == &"code"
            || value.contains("codium")
            || value.contains("visual-studio-code")
            || value.contains("zed")
            || value.contains("rustrover")
            || value.contains("clion")
            || value.contains("pycharm")
            || value.contains("intellij")
            || value.contains("idea")
    })
}

fn process_references_project(window: &RuntimeWindow, project_root: &Path) -> bool {
    if let Some(exe) = window.process_exe.as_deref() {
        let exe = fs::canonicalize(exe).unwrap_or_else(|_| PathBuf::from(exe));
        if exe.starts_with(project_root) {
            return true;
        }
    }

    let Some(pid) = window.pid else {
        return false;
    };
    process_command_line(pid).into_iter().any(|arg| {
        if arg.starts_with('-') {
            return false;
        }
        let path = PathBuf::from(&arg);
        if !path.is_absolute() {
            return false;
        }
        let path = fs::canonicalize(&path).unwrap_or(path);
        path.starts_with(project_root)
    })
}

fn cwd_is_within_project(window: &RuntimeWindow, project_root: &Path) -> bool {
    let Some(cwd) = window.cwd.as_deref() else {
        return false;
    };
    let cwd = fs::canonicalize(cwd).unwrap_or_else(|_| PathBuf::from(cwd));
    cwd.starts_with(project_root)
}

fn is_kitty_window(window: &RuntimeWindow) -> bool {
    window
        .app_id
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .contains("kitty")
        || process_binary_name(window).is_some_and(|name| name.eq_ignore_ascii_case("kitty"))
}

fn code_project_path(window: &RuntimeWindow) -> Option<PathBuf> {
    let pid = window.pid?;
    let args = process_command_line(pid);
    let mut take_next_path = false;
    for arg in args.into_iter().skip(1) {
        if take_next_path {
            let path = PathBuf::from(&arg);
            if path.is_dir() {
                return fs::canonicalize(&path).ok().or(Some(path));
            }
            take_next_path = false;
        }
        if matches!(
            arg.as_str(),
            "--new-window" | "--reuse-window" | "-n" | "-r"
        ) {
            take_next_path = true;
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        let path = PathBuf::from(&arg);
        if path.is_dir() {
            return fs::canonicalize(&path).ok().or(Some(path));
        }
    }
    None
}

fn project_root_from_cwd(cwd: &Path) -> Option<PathBuf> {
    let markers = [
        ".git",
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
    ];
    for ancestor in cwd.ancestors() {
        if markers.iter().any(|marker| ancestor.join(marker).exists()) {
            return fs::canonicalize(ancestor)
                .ok()
                .or_else(|| Some(ancestor.to_path_buf()));
        }
    }
    None
}

fn infer_capture_project_root(windows: &[&RuntimeWindow]) -> Option<PathBuf> {
    for window in windows {
        let app = window
            .app_id
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if app == "code" || app.contains("visual-studio-code") {
            if let Some(path) = code_project_path(window) {
                return Some(path);
            }
        }
    }
    windows.iter().find_map(|window| {
        let cwd = Path::new(window.cwd.as_deref()?);
        project_root_from_cwd(cwd)
    })
}

fn capture_inclusion(
    window: &RuntimeWindow,
    project_root: Option<&Path>,
) -> (bool, Option<String>) {
    let Some(project_root) = project_root else {
        return (true, None);
    };

    if is_terminal_window(window) {
        if cwd_is_within_project(window, project_root) {
            return (
                true,
                Some("terminal belongs to the detected project".to_owned()),
            );
        }
        return (
            false,
            Some(format!(
                "terminal is outside project {}",
                project_root.display()
            )),
        );
    }

    if is_project_editor_window(window) {
        return (
            true,
            Some("editor identifies the detected project".to_owned()),
        );
    }

    if process_references_project(window, project_root) {
        return (
            true,
            Some(
                "process executable or launch arguments reference the detected project".to_owned(),
            ),
        );
    }

    if is_browser_window(window) {
        return (
            true,
            Some("browser window on the project workspace; review before saving".to_owned()),
        );
    }

    (
        false,
        Some(format!(
            "no project context detected for this app (project {})",
            project_root.display()
        )),
    )
}

pub fn apply_capture_selection(recipe: &mut Recipe, included: &[bool]) -> Result<()> {
    if recipe.windows.len() != included.len() {
        bail!("capture selection does not match detected windows");
    }
    let previous = std::mem::take(&mut recipe.windows);
    recipe.windows = previous
        .into_iter()
        .zip(included.iter().copied())
        .filter_map(|(window, include)| include.then_some(window))
        .collect();
    if recipe.windows.is_empty() {
        bail!("select at least one window to save");
    }

    let kept_names: HashSet<&str> = recipe
        .windows
        .iter()
        .map(|window| window.name.as_str())
        .collect();
    if recipe
        .focus
        .as_deref()
        .is_some_and(|focus| !kept_names.contains(focus))
    {
        recipe.focus = None;
    }

    let mut columns = BTreeMap::<usize, usize>::new();
    let mut next_column = 1usize;
    for window in &mut recipe.windows {
        if window.layout.floating {
            continue;
        }
        let old = window.layout.column;
        let new = *columns.entry(old).or_insert_with(|| {
            let current = next_column;
            next_column += 1;
            current
        });
        window.layout.column = new;
    }
    Ok(())
}

fn capture_title_match(window: &RuntimeWindow, duplicate_app_id: bool) -> Option<String> {
    let title = window.title.as_deref()?.trim();
    if title.is_empty() {
        return None;
    }

    let has_app_id = window
        .app_id
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    if has_app_id && !duplicate_app_id {
        return None;
    }

    Some(format!("^{}$", regex::escape(title)))
}

fn pixel_size(value: f64) -> Option<Size> {
    if !value.is_finite() || value < 1.0 || value > f64::from(i32::MAX) {
        return None;
    }
    Some(Size::Pixels(value.round() as i32))
}

fn unique_window_name(window: &RuntimeWindow, index: usize, used: &mut HashSet<String>) -> String {
    let raw = window
        .app_id
        .as_deref()
        .or(window.title.as_deref())
        .unwrap_or("window");
    let base = unique_slug(raw);
    let mut candidate = if base.is_empty() {
        format!("window-{}", index + 1)
    } else {
        base
    };
    let original = candidate.clone();
    let mut suffix = 2;
    while !used.insert(candidate.clone()) {
        candidate = format!("{original}-{suffix}");
        suffix += 1;
    }
    candidate
}

pub fn unique_slug(value: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(ch.to_ascii_lowercase());
        } else {
            dash = true;
        }
    }
    out.trim_matches('-').to_owned()
}

fn infer_command(
    window: &RuntimeWindow,
    cwd: Option<&str>,
    installed_apps: &[InstalledApp],
) -> Vec<String> {
    let app = window
        .app_id
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let title = window.title.as_deref().unwrap_or_default();
    let installed = resolve_installed_app_from(installed_apps, &app);

    if is_kitty_window(window) {
        let mut command = installed
            .map(|app| app.command.clone())
            .unwrap_or_else(|| vec!["kitty".to_owned()]);
        if app != "kitty" {
            if let Some(app_id) = window.app_id.as_ref().filter(|app_id| !app_id.is_empty()) {
                command.extend(["--class".to_owned(), app_id.clone()]);
            }
        }
        if !title.is_empty() {
            command.extend(["--title".to_owned(), title.to_owned()]);
        }
        if let Some(cwd) = cwd {
            command.extend(["--directory".to_owned(), cwd.to_owned()]);
        }
        return command;
    }

    if app == "code" || app.contains("visual-studio-code") {
        let mut command = installed
            .map(|app| app.command.clone())
            .unwrap_or_else(|| vec!["code".to_owned()]);
        command.push("--new-window".to_owned());
        let project = code_project_path(window)
            .or_else(|| cwd.map(PathBuf::from))
            .filter(|path| env::var_os("HOME").as_deref() != Some(path.as_os_str()));
        if let Some(project) = project {
            command.push(project.to_string_lossy().into_owned());
        }
        return command;
    }

    if app.contains("chrome") || app.contains("chromium") {
        return infer_chrome_command(window, installed);
    }

    if app.contains("firefox") {
        if let Some(installed) = installed {
            return installed.command.clone();
        }
        if executable_in_path("firefox") {
            return vec!["firefox".to_owned()];
        }
    }

    if let Some(installed) = installed {
        return installed.command.clone();
    }

    if let Some(exe) = &window.process_exe {
        let path = Path::new(exe);
        if path.is_absolute() && path.exists() {
            return vec![exe.clone()];
        }
    }

    Vec::new()
}

fn infer_chrome_command(window: &RuntimeWindow, installed: Option<&InstalledApp>) -> Vec<String> {
    let mut command = installed.map(|app| app.command.clone()).unwrap_or_else(|| {
        if executable_in_path("flatpak") {
            vec![
                "flatpak".to_owned(),
                "run".to_owned(),
                "com.google.Chrome".to_owned(),
            ]
        } else {
            ["google-chrome-stable", "google-chrome", "chromium"]
                .into_iter()
                .find(|binary| executable_in_path(binary))
                .map(|binary| vec![binary.to_owned()])
                .unwrap_or_default()
        }
    });

    if command.is_empty() {
        return command;
    }

    command.push("--new-window".to_owned());

    let data_root = chrome_user_data_root(window, installed);
    let profile = chrome_profile_directory(window.pid, data_root.as_deref());
    if let Some(profile) = profile.as_deref() {
        command.push(format!("--profile-directory={profile}"));
    }

    if let Some(url) = chrome_active_url(window, data_root.as_deref(), profile.as_deref()) {
        command.push(url);
    }

    command
}

fn decode_process_command_line(bytes: &[u8]) -> Vec<String> {
    let parts = bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect::<Vec<_>>();

    if parts.len() == 1 && parts[0].chars().any(char::is_whitespace) {
        shlex::split(&parts[0]).unwrap_or(parts)
    } else {
        parts
    }
}

fn process_command_line(pid: i32) -> Vec<String> {
    let Ok(bytes) = fs::read(format!("/proc/{pid}/cmdline")) else {
        return Vec::new();
    };
    decode_process_command_line(&bytes)
}

fn chrome_user_data_root(
    window: &RuntimeWindow,
    installed: Option<&InstalledApp>,
) -> Option<PathBuf> {
    if let Some(pid) = window.pid {
        for argument in process_command_line(pid) {
            if let Some(path) = argument.strip_prefix("--user-data-dir=") {
                let path = PathBuf::from(path);
                if path.is_absolute() && path.exists() {
                    return Some(path);
                }
            }
        }
    }

    let home = PathBuf::from(env::var_os("HOME")?);
    if let Some(flatpak_id) = installed.and_then(|app| app.flatpak_id.as_deref()) {
        let candidate = home
            .join(".var/app")
            .join(flatpak_id)
            .join("config/google-chrome");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }

    [
        home.join(".config/google-chrome"),
        home.join(".config/chromium"),
    ]
    .into_iter()
    .find(|candidate| candidate.is_dir())
}

fn chrome_profile_directory(pid: Option<i32>, data_root: Option<&Path>) -> Option<String> {
    if let Some(pid) = pid {
        for argument in process_command_line(pid) {
            if let Some(profile) = argument.strip_prefix("--profile-directory=") {
                if !profile.trim().is_empty() {
                    return Some(profile.to_owned());
                }
            }
        }
    }

    let local_state = data_root?.join("Local State");
    let contents = fs::read_to_string(local_state).ok()?;
    let json: serde_json::Value = serde_json::from_str(&contents).ok()?;
    json.pointer("/profile/last_used")
        .and_then(serde_json::Value::as_str)
        .filter(|profile| !profile.trim().is_empty())
        .map(str::to_owned)
}

fn chrome_active_url(
    window: &RuntimeWindow,
    data_root: Option<&Path>,
    profile: Option<&str>,
) -> Option<String> {
    if !executable_in_path("sqlite3") {
        return None;
    }

    let title = window.title.as_deref()?;
    let title = [" - Google Chrome", " - Chromium"]
        .into_iter()
        .find_map(|suffix| title.strip_suffix(suffix))
        .unwrap_or(title)
        .trim();
    if title.is_empty() {
        return None;
    }

    let profile = profile.unwrap_or("Default");
    let history = data_root?.join(profile).join("History");
    if !history.is_file() {
        return None;
    }

    let snapshot = env::temp_dir().join(format!(
        "niri-workbench-history-{}-{}.sqlite",
        std::process::id(),
        window.id
    ));
    if fs::copy(&history, &snapshot).is_err() {
        return None;
    }

    let escaped = title.replace('\'', "''");
    let query = format!(
        "SELECT url FROM urls WHERE title='{escaped}' ORDER BY last_visit_time DESC LIMIT 1;"
    );
    let output = Command::new("sqlite3")
        .arg("-readonly")
        .arg(&snapshot)
        .arg(query)
        .output()
        .ok();
    let _ = fs::remove_file(&snapshot);

    let output = output?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if url.starts_with("http://") || url.starts_with("https://") {
        Some(url)
    } else {
        None
    }
}

fn executable_in_path(name: &str) -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path)
        .map(|directory| directory.join(name))
        .any(|candidate| candidate.is_file())
}

fn recipe_identity(recipe: &Recipe) -> (&str, &str) {
    (
        recipe.name.as_deref().unwrap_or(&recipe.workspace),
        &recipe.workspace,
    )
}

pub fn upsert_captured_recipe(config: &mut Config, recipe: Recipe) -> (String, usize) {
    let (display_name, workspace) = recipe_identity(&recipe);
    let display_name = display_name.to_owned();
    let workspace = workspace.to_owned();

    let mut duplicate_keys: Vec<String> = config
        .workbench
        .iter()
        .filter(|(_, existing)| {
            let (existing_name, existing_workspace) = recipe_identity(existing);
            existing_name == display_name && existing_workspace == workspace
        })
        .map(|(key, _)| key.clone())
        .collect();
    duplicate_keys.sort();
    let replaced = duplicate_keys.len();

    if let Some(primary) = duplicate_keys.first().cloned() {
        for key in duplicate_keys.iter().skip(1) {
            config.workbench.remove(key);
        }
        config.workbench.insert(primary.clone(), recipe);
        return (primary, replaced);
    }

    let raw_base = unique_slug(&display_name);
    let base = if raw_base.is_empty() {
        "workbench".to_owned()
    } else {
        raw_base
    };
    let mut key = base.clone();
    let mut suffix = 2usize;
    while config.workbench.contains_key(&key) {
        key = format!("{base}-{suffix}");
        suffix += 1;
    }
    config.workbench.insert(key.clone(), recipe);
    (key, replaced)
}

pub fn icon_name(spec: &WindowSpec) -> String {
    if let Some(app_id) = exact_match_app_id(spec) {
        if let Some(icon) = resolve_installed_app(&app_id).and_then(|app| app.icon) {
            return icon;
        }
    }

    if spec.command.first().is_some_and(|part| part == "flatpak")
        && let Some(flatpak_id) = spec.command.get(2)
        && let Some(icon) = resolve_installed_app(flatpak_id).and_then(|app| app.icon)
    {
        return icon;
    }

    let app = spec
        .match_spec
        .app_id
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let command = spec.command.join(" ").to_ascii_lowercase();
    let haystack = format!("{app} {command}");

    if haystack.contains("code") || haystack.contains("vscode") {
        "vscode"
    } else if haystack.contains("kitty") {
        "kitty"
    } else if haystack.contains("chrome") || haystack.contains("com.google.chrome") {
        "google-chrome"
    } else if haystack.contains("firefox") {
        "firefox"
    } else if haystack.contains("android") || haystack.contains("studio") {
        "androidstudio"
    } else if haystack.contains("obsidian") {
        "obsidian"
    } else if haystack.contains("terminal") {
        "utilities-terminal"
    } else if haystack.contains("browser") {
        "web-browser"
    } else {
        "application-x-executable"
    }
    .to_owned()
}

fn exact_match_app_id(spec: &WindowSpec) -> Option<String> {
    let raw = spec.match_spec.app_id.as_deref()?;
    let literal = raw.strip_prefix('^')?.strip_suffix('$')?;
    let mut out = String::new();
    let mut escaped = false;

    for ch in literal.chars() {
        if escaped {
            out.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ".*+?()[]{}|".contains(ch) {
            return None;
        } else {
            out.push(ch);
        }
    }

    (!escaped && !out.is_empty()).then_some(out)
}

pub fn display_command(command: &[String]) -> String {
    command
        .iter()
        .map(|part| {
            if part
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || "/._-:=+%@~".contains(ch))
            {
                part.clone()
            } else {
                format!("{part:?}")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn parse_command(text: &str) -> Result<Vec<String>> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    shlex::split(text).ok_or_else(|| anyhow!("unclosed quote in launch command"))
}

fn cli_program() -> Result<PathBuf> {
    let current = env::current_exe().context("locating niri-workbench-ui")?;
    let sibling = current.with_file_name("niri-workbench");
    Ok(if sibling.exists() {
        sibling
    } else {
        PathBuf::from("niri-workbench")
    })
}

pub fn run_cli(action: &str, key: &str, config_path: &Path) -> Result<CliRunResult> {
    let program = cli_program()?;
    let socket = discover_socket()?;
    let output = Command::new(&program)
        .arg("--config")
        .arg(config_path)
        .arg(action)
        .arg(key)
        .env("NIRI_SOCKET", socket)
        .output()
        .with_context(|| format!("running {} {action} {key}", program.display()))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let detail = match (stdout.is_empty(), stderr.is_empty()) {
        (false, false) => format!("{stdout}\n{stderr}"),
        (false, true) => stdout,
        (true, false) => stderr,
        (true, true) => {
            if output.status.success() {
                format!("{action} completed")
            } else {
                format!("{action} exited with {}", output.status)
            }
        }
    };

    Ok(CliRunResult {
        success: output.status.success(),
        output: detail,
    })
}

pub fn move_window_to_window(
    recipe: &mut Recipe,
    dragged: &str,
    target: &str,
    after: bool,
) -> bool {
    if dragged == target {
        return false;
    }

    let Some(source_index) = recipe
        .windows
        .iter()
        .position(|window| window.name == dragged)
    else {
        return false;
    };

    let mut moved = recipe.windows.remove(source_index);
    let Some(target_index) = recipe
        .windows
        .iter()
        .position(|window| window.name == target)
    else {
        recipe
            .windows
            .insert(source_index.min(recipe.windows.len()), moved);
        return false;
    };

    let target_layout = recipe.windows[target_index].layout.clone();
    moved.layout.column = target_layout.column;
    moved.layout.column_width = None;
    moved.layout.display = target_layout.display;
    moved.layout.floating = false;

    let insert_index = if after {
        target_index + 1
    } else {
        target_index
    };
    recipe.windows.insert(insert_index, moved);
    normalize_columns(recipe);
    true
}

pub fn move_window_to_new_column(recipe: &mut Recipe, name: &str) -> bool {
    let next_column = recipe
        .windows
        .iter()
        .filter(|window| !window.layout.floating && window.name != name)
        .map(|window| window.layout.column)
        .max()
        .unwrap_or(0)
        + 1;

    let Some(window) = recipe.windows.iter_mut().find(|window| window.name == name) else {
        return false;
    };
    window.layout.floating = false;
    window.layout.column = next_column;
    window.layout.column_width = None;
    window.layout.window_height = None;
    window.layout.display = None;
    normalize_columns(recipe);
    true
}

pub fn set_window_floating(recipe: &mut Recipe, name: &str, floating: bool) -> bool {
    let Some(window) = recipe.windows.iter_mut().find(|window| window.name == name) else {
        return false;
    };

    window.layout.floating = floating;
    if floating {
        window.layout.column_width = None;
        window.layout.window_height = None;
        window.layout.display = None;
    }
    normalize_columns(recipe);
    true
}

pub fn remove_window(recipe: &mut Recipe, name: &str) -> Option<String> {
    let position = recipe
        .windows
        .iter()
        .position(|window| window.name == name)?;
    recipe.windows.remove(position);
    if recipe.focus.as_deref() == Some(name) {
        recipe.focus = None;
    }
    normalize_columns(recipe);
    recipe
        .windows
        .get(position)
        .or_else(|| recipe.windows.last())
        .map(|window| window.name.clone())
}

pub fn normalize_columns(recipe: &mut Recipe) {
    let mut existing: Vec<usize> = recipe
        .windows
        .iter()
        .filter(|window| !window.layout.floating)
        .map(|window| window.layout.column)
        .collect();
    existing.sort_unstable();
    existing.dedup();

    let mapping: HashMap<usize, usize> = existing
        .into_iter()
        .enumerate()
        .map(|(index, old)| (old, index + 1))
        .collect();

    for window in &mut recipe.windows {
        if let Some(column) = mapping.get(&window.layout.column) {
            window.layout.column = *column;
        }
    }
}

pub fn set_column_display(recipe: &mut Recipe, column: usize, display: Option<ColumnDisplay>) {
    for window in &mut recipe.windows {
        if window.layout.column == column && !window.layout.floating {
            window.layout.display = display;
        }
    }
}

#[cfg(test)]
mod editor_tests {
    use super::*;

    fn one_window_focus_recipe() -> Recipe {
        Recipe {
            name: Some("focus-test".to_owned()),
            workspace: "Dev".to_owned(),
            output: None,
            output_fallback: OutputFallback::Focused,
            focus: Some("editor".to_owned()),
            spawn_timeout_ms: 10_000,
            windows: vec![window("editor", 1)],
        }
    }

    fn one_window_focus_state(focused_app_id: &str, focused_id: u64) -> ObservedState {
        ObservedState {
            windows: vec![
                RuntimeWindow {
                    id: 1,
                    title: Some("editor".into()),
                    app_id: Some("editor".into()),
                    pid: None,
                    process_exe: None,
                    cwd: None,
                    workspace_id: Some(1),
                    is_focused: focused_id == 1,
                    is_floating: false,
                    column: Some(1),
                    tile_index: Some(1),
                    tile_width: 800.0,
                    tile_height: 600.0,
                },
                RuntimeWindow {
                    id: focused_id,
                    title: Some("other".into()),
                    app_id: Some(focused_app_id.into()),
                    pid: None,
                    process_exe: None,
                    cwd: None,
                    workspace_id: Some(1),
                    is_focused: true,
                    is_floating: true,
                    column: None,
                    tile_index: None,
                    tile_width: 500.0,
                    tile_height: 400.0,
                },
            ],
            workspaces: vec![workbench_core::RuntimeWorkspace {
                id: 1,
                index: 1,
                name: Some("Dev".into()),
                output: None,
                is_active: true,
                is_focused: true,
            }],
            outputs: Vec::new(),
        }
    }

    #[test]
    fn ui_focus_does_not_make_an_otherwise_converged_recipe_look_broken() {
        let recipe = one_window_focus_recipe();
        let state = one_window_focus_state("dev.t1ktak.NiriWorkbench", 99);
        assert_eq!(recipe_status(&recipe, Some(&state)), RecipeStatus::Ready);
    }

    #[test]
    fn non_ui_focus_drift_is_still_reported() {
        let recipe = one_window_focus_recipe();
        let state = one_window_focus_state("unrelated-app", 99);
        assert_eq!(
            recipe_status(&recipe, Some(&state)),
            RecipeStatus::LayoutChanged
        );
    }

    #[test]
    fn parses_flatpak_desktop_entry_for_real_window_identity() {
        let desktop = r#"
[Desktop Entry]
Type=Application
Name=Google Chrome
Exec=/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=/app/bin/chrome --file-forwarding com.google.Chrome @@u %U @@
Icon=com.google.Chrome
StartupWMClass=google-chrome
X-Flatpak=com.google.Chrome
"#;

        let app = parse_desktop_entry("com.google.Chrome", desktop).unwrap();
        assert_eq!(app.name, "Google Chrome");
        assert_eq!(app.icon.as_deref(), Some("com.google.Chrome"));
        assert_eq!(app.startup_wm_class.as_deref(), Some("google-chrome"));
        assert_eq!(app.flatpak_id.as_deref(), Some("com.google.Chrome"));
        assert_eq!(app.command, vec!["flatpak", "run", "com.google.Chrome"]);
    }

    #[test]
    fn installed_app_resolver_matches_startup_wm_class() {
        let apps = vec![InstalledApp {
            desktop_id: "com.google.Chrome".to_owned(),
            name: "Google Chrome".to_owned(),
            icon: Some("com.google.Chrome".to_owned()),
            startup_wm_class: Some("google-chrome".to_owned()),
            flatpak_id: Some("com.google.Chrome".to_owned()),
            command: vec![
                "flatpak".to_owned(),
                "run".to_owned(),
                "com.google.Chrome".to_owned(),
            ],
        }];

        let resolved = resolve_installed_app_from(&apps, "google-chrome").unwrap();
        assert_eq!(resolved.desktop_id, "com.google.Chrome");
    }

    #[test]
    fn desktop_exec_drops_field_codes() {
        assert_eq!(
            desktop_exec_command("code --new-window %F").unwrap(),
            vec!["code", "--new-window"]
        );
        assert_eq!(
            desktop_exec_command("kitty --directory /tmp").unwrap(),
            vec!["kitty", "--directory", "/tmp"]
        );
    }

    fn runtime_window(app_id: Option<&str>, title: &str) -> RuntimeWindow {
        RuntimeWindow {
            id: 7,
            title: Some(title.to_owned()),
            app_id: app_id.map(str::to_owned),
            pid: None,
            process_exe: None,
            cwd: None,
            workspace_id: Some(1),
            is_focused: false,
            is_floating: false,
            column: Some(1),
            tile_index: Some(1),
            tile_width: 800.0,
            tile_height: 600.0,
        }
    }

    #[test]
    fn singleton_capture_match_does_not_depend_on_window_title() {
        let window = runtime_window(Some("code"), "project-a — Visual Studio Code");
        assert_eq!(capture_title_match(&window, false), None);
    }

    #[test]
    fn duplicate_app_capture_uses_title_to_disambiguate_windows() {
        let window = runtime_window(Some("google-chrome"), "docs.rs - Google Chrome");
        assert_eq!(
            capture_title_match(&window, true).as_deref(),
            Some("^docs\\.rs \\- Google Chrome$")
        );
    }

    #[test]
    fn title_is_used_when_window_has_no_app_id() {
        let window = runtime_window(None, "Untyped window");
        assert_eq!(
            capture_title_match(&window, false).as_deref(),
            Some("^Untyped window$")
        );
    }

    #[test]
    fn capture_records_observed_sizes_in_logical_pixels() {
        let mut state = ObservedState::default();
        state.workspaces.push(workbench_core::RuntimeWorkspace {
            id: 1,
            index: 1,
            name: Some("Dev".into()),
            output: Some("eDP-1".into()),
            is_active: true,
            is_focused: true,
        });
        let mut window = runtime_window(Some("code"), "project");
        window.tile_width = 827.4;
        window.tile_height = 1030.0;
        state.windows.push(window);

        let captured = capture_workspace(&state, None, None).unwrap();
        assert_eq!(
            captured.recipe.windows[0].layout.column_width,
            Some(Size::Pixels(827))
        );
        assert_eq!(captured.recipe.windows[0].layout.window_height, None);
    }

    #[test]
    fn app_on_another_workspace_still_forces_disambiguating_title() {
        let mut state = ObservedState::default();
        state.workspaces.extend([
            workbench_core::RuntimeWorkspace {
                id: 1,
                index: 1,
                name: Some("Dev".into()),
                output: None,
                is_active: true,
                is_focused: true,
            },
            workbench_core::RuntimeWorkspace {
                id: 2,
                index: 2,
                name: Some("Other".into()),
                output: None,
                is_active: false,
                is_focused: false,
            },
        ]);
        let mut docs = runtime_window(Some("google-chrome"), "Docs - Google Chrome");
        docs.id = 1;
        docs.workspace_id = Some(1);
        let mut chat = runtime_window(Some("google-chrome"), "Chat - Google Chrome");
        chat.id = 2;
        chat.workspace_id = Some(2);
        state.windows = vec![docs, chat];

        let captured = capture_workspace(&state, Some(1), None).unwrap();
        assert_eq!(captured.recipe.windows.len(), 1);
        assert_eq!(
            captured.recipe.windows[0].match_spec.title.as_deref(),
            Some(r"^Docs \- Google Chrome$")
        );
    }

    #[test]
    fn duplicate_app_capture_prefers_unique_cwd_over_title() {
        let mut state = ObservedState::default();
        state.workspaces.push(workbench_core::RuntimeWorkspace {
            id: 1,
            index: 1,
            name: Some("Dev".into()),
            output: None,
            is_active: true,
            is_focused: true,
        });
        let mut left = runtime_window(Some("kitty"), "changing title a");
        left.id = 1;
        left.cwd = Some("/home/user/project-a".into());
        left.column = Some(1);
        let mut right = runtime_window(Some("kitty"), "changing title b");
        right.id = 2;
        right.cwd = Some("/home/user/project-b".into());
        right.column = Some(2);
        state.windows = vec![left, right];

        let captured = capture_workspace(&state, None, None).unwrap();
        assert_eq!(captured.recipe.windows[0].match_spec.title, None);
        assert_eq!(
            captured.recipe.windows[0].match_spec.cwd.as_deref(),
            Some(r"^/home/user/project\-a$")
        );
        assert_eq!(captured.recipe.windows[1].match_spec.title, None);
        assert_eq!(
            captured.recipe.windows[1].match_spec.cwd.as_deref(),
            Some(r"^/home/user/project\-b$")
        );
    }

    #[test]
    fn capture_upsert_replaces_duplicate_cards() {
        let mut config = Config {
            workbench: BTreeMap::new(),
        };
        let original = recipe();
        config.workbench.insert("test".into(), original.clone());
        config.workbench.insert("test-2".into(), original.clone());
        let mut replacement = original.clone();
        replacement.windows.pop();

        let (key, replaced) = upsert_captured_recipe(&mut config, replacement.clone());
        assert_eq!(key, "test");
        assert_eq!(replaced, 2);
        assert_eq!(config.workbench.len(), 1);
        assert_eq!(config.workbench.get("test"), Some(&replacement));
    }

    #[test]
    fn custom_kitty_class_recreates_class_title_and_cwd() {
        let mut window = runtime_window(Some("nwb-project-terminal"), "project terminal");
        window.process_exe = Some("/usr/bin/kitty".into());
        window.cwd = Some("/home/user/project".into());
        let command = infer_command(&window, window.cwd.as_deref(), &[]);
        assert_eq!(
            command,
            vec![
                "kitty".to_owned(),
                "--class".to_owned(),
                "nwb-project-terminal".to_owned(),
                "--title".to_owned(),
                "project terminal".to_owned(),
                "--directory".to_owned(),
                "/home/user/project".to_owned(),
            ]
        );
    }

    #[test]
    fn terminal_detection_uses_process_executable_for_custom_classes() {
        let mut window = runtime_window(Some("project-shell"), "shell");
        window.process_exe = Some("/usr/bin/kitty".into());
        assert!(is_terminal_window(&window));
        assert!(is_kitty_window(&window));
    }

    #[test]
    fn electron_style_single_string_cmdline_is_split() {
        assert_eq!(
            decode_process_command_line(b"/usr/share/code/code --new-window /home/user/project\0"),
            vec![
                "/usr/share/code/code".to_owned(),
                "--new-window".to_owned(),
                "/home/user/project".to_owned(),
            ]
        );
        assert_eq!(
            decode_process_command_line(b"code\0--new-window\0/home/user/project\0"),
            vec![
                "code".to_owned(),
                "--new-window".to_owned(),
                "/home/user/project".to_owned(),
            ]
        );
    }

    #[test]
    fn capture_selection_removes_windows_and_compacts_columns() {
        let mut recipe = recipe();
        recipe.windows[0].layout.column = 1;
        recipe.windows[1].layout.column = 3;
        recipe.windows[2].layout.column = 4;
        recipe.focus = Some("editor".into());

        apply_capture_selection(&mut recipe, &[true, false, true]).unwrap();
        assert_eq!(recipe.windows.len(), 2);
        assert_eq!(recipe.windows[0].layout.column, 1);
        assert_eq!(recipe.windows[1].layout.column, 2);
        assert_eq!(recipe.focus.as_deref(), Some("editor"));
    }

    #[test]
    fn unrelated_non_project_app_is_excluded_when_project_is_known() {
        let root = Path::new("/home/user/project");
        let mut system_app = runtime_window(Some("org.pulseaudio.pavucontrol"), "Volume Control");
        system_app.cwd = Some("/home/user/project".into());
        system_app.process_exe = Some("/usr/bin/pavucontrol".into());
        assert!(!capture_inclusion(&system_app, Some(root)).0);
    }

    #[test]
    fn project_local_executable_is_included() {
        let root = Path::new("/home/user/project");
        let mut app = runtime_window(Some("project-preview"), "Project preview");
        app.cwd = Some("/home/user/project".into());
        app.process_exe = Some("/home/user/project/target/debug/project-preview".into());
        assert!(capture_inclusion(&app, Some(root)).0);
    }

    #[test]
    fn browser_is_kept_as_reviewable_project_companion() {
        let root = Path::new("/home/user/project");
        let mut browser = runtime_window(Some("google-chrome"), "Docs - Google Chrome");
        browser.cwd = Some("/home/user".into());
        assert!(capture_inclusion(&browser, Some(root)).0);
    }

    #[test]
    fn generic_workspace_without_project_root_keeps_windows() {
        let mut steam = runtime_window(Some("steam"), "Steam");
        steam.cwd = Some("/home/user".into());
        assert!(capture_inclusion(&steam, None).0);
    }

    #[test]
    fn project_terminal_is_included_but_unrelated_terminal_is_not() {
        let root = Path::new("/home/user/project");
        let mut project = runtime_window(Some("kitty"), "project shell");
        project.cwd = Some("/home/user/project".into());
        let mut unrelated = runtime_window(Some("Alacritty"), "other shell");
        unrelated.cwd = Some("/tmp/other".into());

        assert!(capture_inclusion(&project, Some(root)).0);
        assert!(!capture_inclusion(&unrelated, Some(root)).0);
    }

    #[test]
    fn exact_match_app_id_unescapes_capture_regex() {
        let spec = WindowSpec {
            name: "chrome".to_owned(),
            command: Vec::new(),
            match_spec: MatchSpec {
                app_id: Some("^google\\-chrome$".to_owned()),
                ..MatchSpec::default()
            },
            reuse: ReusePolicy::Unique,
            layout: PlacementSpec::default(),
        };
        assert_eq!(exact_match_app_id(&spec).as_deref(), Some("google-chrome"));
    }

    fn window(name: &str, column: usize) -> WindowSpec {
        WindowSpec {
            name: name.to_owned(),
            command: vec!["true".to_owned()],
            match_spec: MatchSpec {
                app_id: Some(format!("^{name}$")),
                ..MatchSpec::default()
            },
            reuse: ReusePolicy::Unique,
            layout: PlacementSpec {
                column,
                ..PlacementSpec::default()
            },
        }
    }

    fn recipe() -> Recipe {
        Recipe {
            name: Some("test".to_owned()),
            workspace: "Dev".to_owned(),
            output: None,
            output_fallback: OutputFallback::Focused,
            focus: Some("editor".to_owned()),
            spawn_timeout_ms: 10_000,
            windows: vec![
                window("editor", 1),
                window("terminal", 2),
                window("browser", 2),
            ],
        }
    }

    #[test]
    fn moving_window_onto_another_window_moves_it_to_that_column() {
        let mut recipe = recipe();
        assert!(move_window_to_window(
            &mut recipe,
            "editor",
            "terminal",
            true
        ));

        let editor = recipe
            .windows
            .iter()
            .find(|window| window.name == "editor")
            .unwrap();
        let terminal = recipe
            .windows
            .iter()
            .find(|window| window.name == "terminal")
            .unwrap();

        assert_eq!(editor.layout.column, terminal.layout.column);
        assert!(!editor.layout.floating);
        assert_eq!(
            recipe
                .windows
                .iter()
                .map(|window| window.name.as_str())
                .collect::<Vec<_>>(),
            vec!["terminal", "editor", "browser"]
        );
    }

    #[test]
    fn new_column_places_window_after_existing_tiled_columns() {
        let mut recipe = recipe();
        assert!(move_window_to_new_column(&mut recipe, "terminal"));

        let terminal = recipe
            .windows
            .iter()
            .find(|window| window.name == "terminal")
            .unwrap();
        assert_eq!(terminal.layout.column, 3);
        assert!(!terminal.layout.floating);
    }

    #[test]
    fn floating_clears_tiling_only_properties() {
        let mut recipe = recipe();
        let terminal = recipe
            .windows
            .iter_mut()
            .find(|window| window.name == "terminal")
            .unwrap();
        terminal.layout.column_width = Some(Size::Percent(0.4));
        terminal.layout.window_height = Some(Size::Percent(0.5));
        terminal.layout.display = Some(ColumnDisplay::Tabbed);

        assert!(set_window_floating(&mut recipe, "terminal", true));
        let terminal = recipe
            .windows
            .iter()
            .find(|window| window.name == "terminal")
            .unwrap();

        assert!(terminal.layout.floating);
        assert_eq!(terminal.layout.column_width, None);
        assert_eq!(terminal.layout.window_height, None);
        assert_eq!(terminal.layout.display, None);
    }

    #[test]
    fn removing_focused_window_clears_focus_and_selects_neighbor() {
        let mut recipe = recipe();
        let next = remove_window(&mut recipe, "editor");

        assert_eq!(recipe.focus, None);
        assert_eq!(next.as_deref(), Some("terminal"));
        assert!(recipe.windows.iter().all(|window| window.name != "editor"));
        assert_eq!(
            recipe
                .windows
                .iter()
                .filter(|window| !window.layout.floating)
                .map(|window| window.layout.column)
                .collect::<HashSet<_>>(),
            HashSet::from([1])
        );
    }

    #[test]
    fn tabbed_mode_applies_to_entire_tiled_column_only() {
        let mut recipe = recipe();
        recipe.windows.push(WindowSpec {
            name: "floating".to_owned(),
            command: Vec::new(),
            match_spec: MatchSpec::default(),
            reuse: ReusePolicy::Unique,
            layout: PlacementSpec {
                column: 2,
                floating: true,
                ..PlacementSpec::default()
            },
        });

        set_column_display(&mut recipe, 2, Some(ColumnDisplay::Tabbed));

        for window in &recipe.windows {
            if window.layout.column == 2 && !window.layout.floating {
                assert_eq!(window.layout.display, Some(ColumnDisplay::Tabbed));
            }
        }
        assert_eq!(
            recipe
                .windows
                .iter()
                .find(|window| window.name == "floating")
                .unwrap()
                .layout
                .display,
            None
        );
    }
}
