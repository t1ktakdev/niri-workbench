use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use niri_ipc::{Action as NiriAction, SizeChange};
use workbench_core::{
    CandidateDecision, ColumnDisplay, Config, MatchSpec, ObservedState, OutputFallback,
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
            let Some(window_id) = snapshot
                .windows
                .iter()
                .find(|window| window.pid == Some(pid))
                .map(|window| window.id)
            else {
                return;
            };

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

            for action in [
                NiriAction::SetWindowWidth {
                    id: Some(window_id),
                    change: SizeChange::SetFixed(width),
                },
                NiriAction::SetWindowHeight {
                    id: Some(window_id),
                    change: SizeChange::SetFixed(height),
                },
                NiriAction::CenterWindow {
                    id: Some(window_id),
                },
            ] {
                if session.send_action(action).await.is_err() {
                    return;
                }
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
    if plan.actions.is_empty() {
        RecipeStatus::Ready
    } else {
        RecipeStatus::LayoutChanged
    }
}

pub fn capture_workspace(
    state: &ObservedState,
    preferred_workspace_id: Option<u64>,
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

    let output_size = workspace
        .output
        .as_deref()
        .and_then(|name| state.output(name))
        .and_then(|output| Some((f64::from(output.width?), f64::from(output.height?))));

    let workspace_name = workspace
        .name
        .clone()
        .unwrap_or_else(|| format!("Workspace {}", workspace.index));
    let mut seen_names = HashSet::new();
    let mut seen_columns = HashSet::new();
    let mut specs = Vec::new();
    let mut details = Vec::new();
    let mut focus = None;

    for (index, window) in windows.iter().enumerate() {
        let logical = unique_window_name(window, index, &mut seen_names);
        if window.is_focused {
            focus = Some(logical.clone());
        }

        let cwd = window.pid.and_then(process_cwd);
        let title = window.title.clone().unwrap_or_else(|| logical.clone());
        let app_id = window.app_id.clone().unwrap_or_default();

        let mut placement = PlacementSpec {
            column: window.column.unwrap_or(index + 1),
            floating: window.is_floating,
            ..PlacementSpec::default()
        };

        if !window.is_floating {
            if let Some((output_width, output_height)) = output_size {
                if seen_columns.insert(placement.column) && output_width > 0.0 {
                    placement.column_width =
                        ratio_size(window.tile_width / output_width, 0.05, 1.0);
                }
                if column_counts.get(&placement.column).copied().unwrap_or(1) > 1
                    && output_height > 0.0
                {
                    placement.window_height =
                        ratio_size(window.tile_height / output_height, 0.05, 1.0);
                }
            }
        }

        let match_spec = MatchSpec {
            window_id: None,
            app_id: window
                .app_id
                .as_ref()
                .map(|value| format!("^{}$", regex::escape(value))),
            title: window
                .title
                .as_ref()
                .filter(|title| !title.trim().is_empty())
                .map(|value| format!("^{}$", regex::escape(value))),
            process: None,
            pid: None,
        };

        specs.push(WindowSpec {
            name: logical,
            command: infer_command(window, cwd.as_deref()),
            match_spec,
            reuse: ReusePolicy::Unique,
            layout: placement,
        });
        details.push(CapturedWindow { title, app_id, cwd });
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

fn ratio_size(value: f64, min: f64, max: f64) -> Option<Size> {
    if !value.is_finite() {
        return None;
    }
    Some(Size::Percent(value.clamp(min, max)))
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

fn process_cwd(pid: i32) -> Option<String> {
    fs::read_link(format!("/proc/{pid}/cwd"))
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

fn infer_command(window: &RuntimeWindow, cwd: Option<&str>) -> Vec<String> {
    let app = window
        .app_id
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let title = window.title.as_deref().unwrap_or_default();

    if app.contains("kitty") {
        let mut command = vec!["kitty".to_owned()];
        if app != "kitty" && !app.is_empty() {
            command.extend(["--class".to_owned(), window.app_id.clone().unwrap()]);
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
        let mut command = vec!["code".to_owned(), "--new-window".to_owned()];
        if let Some(cwd) = cwd {
            if env::var_os("HOME").as_deref() != Some(std::ffi::OsStr::new(cwd)) {
                command.push(cwd.to_owned());
            }
        }
        return command;
    }

    if app.contains("chrome") {
        if executable_in_path("flatpak") {
            return vec![
                "flatpak".to_owned(),
                "run".to_owned(),
                "com.google.Chrome".to_owned(),
            ];
        }
        for binary in ["google-chrome-stable", "google-chrome", "chromium"] {
            if executable_in_path(binary) {
                return vec![binary.to_owned()];
            }
        }
    }

    if app.contains("firefox") && executable_in_path("firefox") {
        return vec!["firefox".to_owned()];
    }

    if let Some(exe) = &window.process_exe {
        let path = Path::new(exe);
        if path.is_absolute() && path.exists() {
            return vec![exe.clone()];
        }
    }

    Vec::new()
}

fn executable_in_path(name: &str) -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path)
        .map(|directory| directory.join(name))
        .any(|candidate| candidate.is_file())
}

pub fn icon_name(spec: &WindowSpec) -> &'static str {
    let app = spec
        .match_spec
        .app_id
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let command = spec
        .command
        .first()
        .map(String::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
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

pub fn launch_cli(action: &str, key: &str) -> Result<()> {
    let current = env::current_exe().context("locating niri-workbench-ui")?;
    let sibling = current.with_file_name("niri-workbench");
    let program = if sibling.exists() {
        sibling
    } else {
        PathBuf::from("niri-workbench")
    };

    Command::new(&program)
        .arg(action)
        .arg(key)
        .spawn()
        .with_context(|| format!("launching {} {action} {key}", program.display()))?;
    Ok(())
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
