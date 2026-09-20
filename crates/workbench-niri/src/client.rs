use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use niri_ipc::{
    Action, Output, Reply, Request, Response, Window, WindowLayout, Workspace,
    WorkspaceReferenceArg,
};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::process::Command;
use tokio::sync::{Mutex, Notify, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout_at};
use tracing::{debug, warn};
use workbench_core::{
    CandidateDecision, MatchError, ObservedState, OutputFallback, OutputInfo, Recipe,
    RuntimeWindow, RuntimeWorkspace, WindowSpec, choose_candidate,
};

#[derive(Debug, Error)]
pub enum NiriError {
    #[error("NIRI_SOCKET is not set; run inside a Niri session or pass --socket")]
    MissingSocket,
    #[error("could not connect to Niri socket {path}: {source}")]
    Connect {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Niri IPC I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid JSON from Niri: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Niri rejected request: {0}")]
    Rejected(String),
    #[error("unexpected Niri response: {0:?}")]
    Unexpected(Box<Response>),
    #[error("event stream ended before initial window/workspace state arrived")]
    IncompleteInitialState,
    #[error("Niri EventStream disconnected while waiting for state convergence")]
    EventStreamClosed,
    #[error("timed out waiting for Niri state to change")]
    StateTimeout,
    #[error("requested output {0:?} is not connected")]
    MissingOutput(String),
    #[error("could not determine a focused/connected output")]
    NoOutput,
    #[error("could not find an empty workspace on output {0:?}")]
    NoEmptyWorkspace(String),
    #[error("workspace operation did not converge: {0}")]
    WorkspaceDidNotConverge(String),
}

#[derive(Debug, Error)]
pub enum SpawnError {
    #[error("window {name:?} is missing and has no spawn command")]
    MissingCommand { name: String },
    #[error("could not expand argument {arg:?} for window {name:?}: {message}")]
    Expand {
        name: String,
        arg: String,
        message: String,
    },
    #[error("could not launch window {name:?} with command {command:?}: {source}")]
    Launch {
        name: String,
        command: Vec<String>,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "could not resolve window {name:?} within {timeout_ms}ms after spawning {command:?}\n\
         matcher: {matcher}\nobserved new candidates:\n{candidates}"
    )]
    Timeout {
        name: String,
        command: Vec<String>,
        timeout_ms: u64,
        matcher: String,
        candidates: String,
    },
    #[error("matcher error while waiting for {name:?}: {message}")]
    Matcher { name: String, message: String },
    #[error("Niri EventStream disconnected while waiting for window {name:?}")]
    EventStreamClosed { name: String },
}

#[derive(Debug, Clone)]
pub struct TargetOutput {
    pub name: String,
    pub fallback_warning: Option<String>,
}

struct IpcConnection {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl IpcConnection {
    async fn connect(path: &Path) -> Result<Self, NiriError> {
        let stream = UnixStream::connect(path)
            .await
            .map_err(|source| NiriError::Connect {
                path: path.display().to_string(),
                source,
            })?;
        let (reader, writer) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(reader),
            writer,
        })
    }

    async fn send(&mut self, request: Request) -> Result<Response, NiriError> {
        let mut payload = serde_json::to_vec(&request)?;
        payload.push(b'\n');
        self.writer.write_all(&payload).await?;
        self.writer.flush().await?;

        let mut line = String::new();
        let count = self.reader.read_line(&mut line).await?;
        if count == 0 {
            return Err(NiriError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Niri closed IPC connection",
            )));
        }
        let reply: Reply = serde_json::from_str(&line)?;
        reply.map_err(NiriError::Rejected)
    }
}

pub struct NiriSession {
    socket_path: PathBuf,
    command: Mutex<IpcConnection>,
    state: Arc<RwLock<ObservedState>>,
    version: Arc<AtomicU64>,
    layout_version: Arc<AtomicU64>,
    event_closed: Arc<AtomicBool>,
    notify: Arc<Notify>,
    _event_task: JoinHandle<()>,
}

impl NiriSession {
    pub fn default_socket_path() -> Result<PathBuf, NiriError> {
        std::env::var_os("NIRI_SOCKET")
            .map(PathBuf::from)
            .ok_or(NiriError::MissingSocket)
    }

    pub async fn connect(path: impl Into<PathBuf>) -> Result<Self, NiriError> {
        let socket_path = path.into();
        let command = IpcConnection::connect(&socket_path).await?;
        let state = Arc::new(RwLock::new(ObservedState::default()));
        let version = Arc::new(AtomicU64::new(0));
        let layout_version = Arc::new(AtomicU64::new(0));
        let event_closed = Arc::new(AtomicBool::new(false));
        let notify = Arc::new(Notify::new());
        let event_task = start_event_stream(
            &socket_path,
            Arc::clone(&state),
            Arc::clone(&version),
            Arc::clone(&layout_version),
            Arc::clone(&event_closed),
            Arc::clone(&notify),
        )
        .await?;

        let session = Self {
            socket_path,
            command: Mutex::new(command),
            state,
            version,
            layout_version,
            event_closed,
            notify,
            _event_task: event_task,
        };
        session.refresh_outputs().await?;
        Ok(session)
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub async fn snapshot(&self) -> ObservedState {
        let mut snapshot = self.state.read().await.clone();
        for window in &mut snapshot.windows {
            refresh_process_metadata(window);
        }
        snapshot
    }

    pub async fn version_string(&self) -> Result<String, NiriError> {
        match self.send_request(Request::Version).await? {
            Response::Version(version) => Ok(version),
            other => Err(NiriError::Unexpected(Box::new(other))),
        }
    }

    pub async fn refresh_outputs(&self) -> Result<Vec<OutputInfo>, NiriError> {
        let outputs = match self.send_request(Request::Outputs).await? {
            Response::Outputs(outputs) => outputs,
            other => return Err(NiriError::Unexpected(Box::new(other))),
        };
        let converted = convert_outputs(outputs);
        self.state.write().await.outputs.clone_from(&converted);
        Ok(converted)
    }

    pub async fn send_action(&self, action: Action) -> Result<(), NiriError> {
        match self.send_request(Request::Action(action)).await? {
            Response::Handled => Ok(()),
            other => Err(NiriError::Unexpected(Box::new(other))),
        }
    }

    async fn send_request(&self, request: Request) -> Result<Response, NiriError> {
        self.command.lock().await.send(request).await
    }

    pub async fn resolve_target_output(&self, recipe: &Recipe) -> Result<TargetOutput, NiriError> {
        self.refresh_outputs().await?;
        let snapshot = self.snapshot().await;
        resolve_target_output_from_state(recipe, &snapshot)
    }

    pub async fn ensure_workspace(&self, name: &str, output: &str) -> Result<u64, NiriError> {
        if let Some(workspace) = self
            .snapshot()
            .await
            .workspaces
            .into_iter()
            .find(|w| w.name.as_deref() == Some(name))
        {
            if workspace.output.as_deref() != Some(output) {
                let before = self.version.load(Ordering::SeqCst);
                self.send_action(Action::MoveWorkspaceToMonitor {
                    output: output.to_owned(),
                    reference: Some(WorkspaceReferenceArg::Id(workspace.id)),
                })
                .await?;
                self.wait_for_change_after(before, Duration::from_secs(2))
                    .await?;
                self.wait_until(Duration::from_secs(2), |state| {
                    state
                        .workspaces
                        .iter()
                        .any(|w| w.id == workspace.id && w.output.as_deref() == Some(output))
                })
                .await
                .map_err(|_| {
                    NiriError::WorkspaceDidNotConverge(format!(
                        "workspace {name:?} did not move to output {output:?}"
                    ))
                })?;
            }
            return Ok(workspace.id);
        }

        let snapshot = self.snapshot().await;
        let occupied: HashSet<u64> = snapshot
            .windows
            .iter()
            .filter_map(|w| w.workspace_id)
            .collect();
        let empty = snapshot
            .workspaces
            .iter()
            .filter(|w| w.output.as_deref() == Some(output))
            .filter(|w| !occupied.contains(&w.id))
            .max_by_key(|w| w.index)
            .ok_or_else(|| NiriError::NoEmptyWorkspace(output.to_owned()))?;

        let id = empty.id;
        let before = self.version.load(Ordering::SeqCst);
        self.send_action(Action::SetWorkspaceName {
            name: name.to_owned(),
            workspace: Some(WorkspaceReferenceArg::Id(id)),
        })
        .await?;
        self.wait_for_change_after(before, Duration::from_secs(2))
            .await?;
        self.wait_until(Duration::from_secs(2), |state| {
            state
                .workspaces
                .iter()
                .any(|w| w.id == id && w.name.as_deref() == Some(name))
        })
        .await
        .map_err(|_| {
            NiriError::WorkspaceDidNotConverge(format!(
                "empty workspace {id} was not named {name:?}"
            ))
        })?;
        Ok(id)
    }

    pub async fn spawn_and_wait(
        &self,
        spec: &WindowSpec,
        used: &HashSet<u64>,
        timeout_ms: u64,
    ) -> Result<u64, SpawnError> {
        if spec.command.is_empty() {
            return Err(SpawnError::MissingCommand {
                name: spec.name.clone(),
            });
        }

        let command = expand_command(spec)?;
        let baseline: HashSet<u64> = self
            .snapshot()
            .await
            .windows
            .iter()
            .map(|window| window.id)
            .collect();

        debug!(window = %spec.name, ?command, "spawning missing window");
        let mut launch = Command::new(&command[0]);
        launch.args(&command[1..]);
        apply_niri_session_environment(&mut launch, &self.socket_path);
        let mut child = launch
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(false)
            .spawn()
            .map_err(|source| SpawnError::Launch {
                name: spec.name.clone(),
                command: command.clone(),
                source,
            })?;
        let child_pid = child.id();
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        debug!(window = %spec.name, ?child_pid, "spawn command started");

        let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(10));
        let mut last_version = self.version.load(Ordering::SeqCst);
        loop {
            if self.event_closed.load(Ordering::SeqCst) {
                return Err(SpawnError::EventStreamClosed {
                    name: spec.name.clone(),
                });
            }
            let snapshot = self.snapshot().await;
            match choose_spawn_candidate(spec, &snapshot.windows, used, &baseline, child_pid)
                .map_err(|err| SpawnError::Matcher {
                    name: spec.name.clone(),
                    message: err.to_string(),
                })? {
                CandidateDecision::One(_candidate) => {
                    // A short event-driven settle window catches launchers that create two
                    // matching toplevels back-to-back without resorting to fixed multi-second sleeps.
                    let settle_deadline = Instant::now() + Duration::from_millis(120);
                    let _ = timeout_at(settle_deadline, self.notify.notified()).await;
                    let snapshot = self.snapshot().await;
                    match choose_spawn_candidate(
                        spec,
                        &snapshot.windows,
                        used,
                        &baseline,
                        child_pid,
                    )
                    .map_err(|err| SpawnError::Matcher {
                        name: spec.name.clone(),
                        message: err.to_string(),
                    })? {
                        CandidateDecision::One(settled) => return Ok(settled.id),
                        CandidateDecision::None => continue,
                        CandidateDecision::Ambiguous(_) => {}
                    }
                }
                CandidateDecision::None | CandidateDecision::Ambiguous(_) => {}
            }

            if Instant::now() >= deadline {
                let snapshot = self.snapshot().await;
                let candidates = describe_new_candidates(spec, &snapshot, &baseline, used);
                return Err(SpawnError::Timeout {
                    name: spec.name.clone(),
                    command,
                    timeout_ms,
                    matcher: describe_matcher(spec),
                    candidates,
                });
            }

            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.version.load(Ordering::SeqCst) == last_version {
                let _ = timeout_at(deadline, notified).await;
            }
            last_version = self.version.load(Ordering::SeqCst);
        }
    }

    pub async fn wait_for_change_after(
        &self,
        before: u64,
        timeout: Duration,
    ) -> Result<(), NiriError> {
        if self.version.load(Ordering::SeqCst) > before {
            return Ok(());
        }
        if self.event_closed.load(Ordering::SeqCst) {
            return Err(NiriError::EventStreamClosed);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.version.load(Ordering::SeqCst) > before {
                return Ok(());
            }
            if self.event_closed.load(Ordering::SeqCst) {
                return Err(NiriError::EventStreamClosed);
            }
            timeout_at(deadline, notified)
                .await
                .map_err(|_| NiriError::StateTimeout)?;
            if self.version.load(Ordering::SeqCst) > before {
                return Ok(());
            }
        }
    }

    pub async fn wait_until<F>(&self, timeout: Duration, predicate: F) -> Result<(), NiriError>
    where
        F: Fn(&ObservedState) -> bool,
    {
        let deadline = Instant::now() + timeout;
        loop {
            if self.event_closed.load(Ordering::SeqCst) {
                return Err(NiriError::EventStreamClosed);
            }
            let observed_version = self.version.load(Ordering::SeqCst);
            if predicate(&self.snapshot().await) {
                return Ok(());
            }
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.version.load(Ordering::SeqCst) != observed_version {
                continue;
            }
            if predicate(&self.snapshot().await) {
                return Ok(());
            }
            timeout_at(deadline, notified)
                .await
                .map_err(|_| NiriError::StateTimeout)?;
        }
    }

    pub async fn wait_for_layout_change_after(
        &self,
        before: u64,
        timeout: Duration,
    ) -> Result<(), NiriError> {
        if self.layout_version.load(Ordering::SeqCst) > before {
            return Ok(());
        }
        if self.event_closed.load(Ordering::SeqCst) {
            return Err(NiriError::EventStreamClosed);
        }

        let deadline = Instant::now() + timeout;
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            if self.layout_version.load(Ordering::SeqCst) > before {
                return Ok(());
            }
            if self.event_closed.load(Ordering::SeqCst) {
                return Err(NiriError::EventStreamClosed);
            }

            timeout_at(deadline, notified)
                .await
                .map_err(|_| NiriError::StateTimeout)?;
        }
    }

    pub fn state_version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }

    pub fn layout_version(&self) -> u64 {
        self.layout_version.load(Ordering::SeqCst)
    }
}

fn resolve_target_output_from_state(
    recipe: &Recipe,
    snapshot: &ObservedState,
) -> Result<TargetOutput, NiriError> {
    let requested = recipe
        .output
        .clone()
        .or_else(|| recipe.windows.iter().find_map(|w| w.layout.output.clone()));

    if let Some(requested) = requested {
        if snapshot
            .outputs
            .iter()
            .any(|o| o.name == requested && o.width.is_some())
        {
            return Ok(TargetOutput {
                name: requested,
                fallback_warning: None,
            });
        }
        if recipe.output_fallback == OutputFallback::Error {
            return Err(NiriError::MissingOutput(requested));
        }
        let fallback = focused_output_name(snapshot).ok_or(NiriError::NoOutput)?;
        return Ok(TargetOutput {
            fallback_warning: Some(format!(
                "requested output {requested:?} is not connected; using focused output {fallback:?}"
            )),
            name: fallback,
        });
    }

    let name = focused_output_name(snapshot)
        .or_else(|| {
            snapshot
                .outputs
                .iter()
                .find(|o| o.width.is_some())
                .map(|o| o.name.clone())
        })
        .ok_or(NiriError::NoOutput)?;
    Ok(TargetOutput {
        name,
        fallback_warning: None,
    })
}

fn focused_output_name(state: &ObservedState) -> Option<String> {
    state
        .focused_workspace()
        .and_then(|workspace| workspace.output.clone())
}

fn expand_command(spec: &WindowSpec) -> Result<Vec<String>, SpawnError> {
    spec.command
        .iter()
        .map(|arg| {
            shellexpand::full(arg)
                .map(|expanded| expanded.into_owned())
                .map_err(|err| SpawnError::Expand {
                    name: spec.name.clone(),
                    arg: arg.clone(),
                    message: err.to_string(),
                })
        })
        .collect()
}

fn choose_spawn_candidate(
    spec: &WindowSpec,
    windows: &[RuntimeWindow],
    used: &HashSet<u64>,
    baseline: &HashSet<u64>,
    spawned_pid: Option<u32>,
) -> Result<CandidateDecision, MatchError> {
    let decision = choose_candidate(spec, windows, used, Some(baseline))?;
    let CandidateDecision::Ambiguous(candidates) = &decision else {
        return Ok(decision);
    };
    let Some(pid) = spawned_pid.and_then(|pid| i32::try_from(pid).ok()) else {
        return Ok(decision);
    };

    let mut matching_pid = candidates
        .iter()
        .filter(|candidate| candidate.pid == Some(pid));
    let Some(candidate) = matching_pid.next().cloned() else {
        return Ok(decision);
    };
    if matching_pid.next().is_some() {
        return Ok(decision);
    }

    Ok(CandidateDecision::One(candidate))
}

fn describe_matcher(spec: &WindowSpec) -> String {
    let mut fields = Vec::new();
    if let Some(value) = spec.match_spec.window_id {
        fields.push(format!("window_id={value}"));
    }
    if let Some(value) = &spec.match_spec.app_id {
        fields.push(format!("app_id={value:?}"));
    }
    if let Some(value) = &spec.match_spec.title {
        fields.push(format!("title={value:?}"));
    }
    if let Some(value) = &spec.match_spec.process {
        fields.push(format!("process={value:?}"));
    }
    if let Some(value) = spec.match_spec.pid {
        fields.push(format!("pid={value}"));
    }
    fields.join(", ")
}

fn describe_new_candidates(
    spec: &WindowSpec,
    state: &ObservedState,
    baseline: &HashSet<u64>,
    used: &HashSet<u64>,
) -> String {
    match choose_candidate(spec, &state.windows, used, Some(baseline)) {
        Ok(CandidateDecision::Ambiguous(candidates)) => candidates
            .iter()
            .map(|candidate| {
                format!(
                    "  id={} app_id={:?} title={:?} pid={:?} process={:?}",
                    candidate.id,
                    candidate.app_id,
                    candidate.title,
                    candidate.pid,
                    candidate.process_exe
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Ok(CandidateDecision::One(candidate)) => format!(
            "  id={} app_id={:?} title={:?} pid={:?} process={:?}",
            candidate.id, candidate.app_id, candidate.title, candidate.pid, candidate.process_exe
        ),
        _ => "  (none matched)".to_owned(),
    }
}

async fn start_event_stream(
    path: &Path,
    state: Arc<RwLock<ObservedState>>,
    version: Arc<AtomicU64>,
    layout_version: Arc<AtomicU64>,
    event_closed: Arc<AtomicBool>,
    notify: Arc<Notify>,
) -> Result<JoinHandle<()>, NiriError> {
    let stream = UnixStream::connect(path)
        .await
        .map_err(|source| NiriError::Connect {
            path: path.display().to_string(),
            source,
        })?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let mut payload = serde_json::to_vec(&Request::EventStream)?;
    payload.push(b'\n');
    writer.write_all(&payload).await?;
    writer.flush().await?;

    let mut line = String::new();
    if reader.read_line(&mut line).await? == 0 {
        return Err(NiriError::IncompleteInitialState);
    }
    let reply: Reply = serde_json::from_str(&line)?;
    match reply.map_err(NiriError::Rejected)? {
        Response::Handled => {}
        other => return Err(NiriError::Unexpected(Box::new(other))),
    }

    let mut saw_workspaces = false;
    let mut saw_windows = false;
    while !(saw_workspaces && saw_windows) {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            return Err(NiriError::IncompleteInitialState);
        }
        let value: serde_json::Value = serde_json::from_str(&line)?;
        saw_workspaces |= value.get("WorkspacesChanged").is_some();
        saw_windows |= value.get("WindowsChanged").is_some();
        let is_layout_change = value.get("WindowLayoutsChanged").is_some();
        apply_event_value(&value, &state).await;
        if is_layout_change {
            layout_version.fetch_add(1, Ordering::SeqCst);
        }
        version.fetch_add(1, Ordering::SeqCst);
        notify.notify_waiters();
    }

    Ok(tokio::spawn(async move {
        let _writer = writer;
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    warn!("Niri event stream closed");
                    event_closed.store(true, Ordering::SeqCst);
                    notify.notify_waiters();
                    return;
                }
                Ok(_) => match serde_json::from_str::<serde_json::Value>(&line) {
                    Ok(value) => {
                        let is_layout_change = value.get("WindowLayoutsChanged").is_some();
                        apply_event_value(&value, &state).await;
                        if is_layout_change {
                            layout_version.fetch_add(1, Ordering::SeqCst);
                        }
                        version.fetch_add(1, Ordering::SeqCst);
                        notify.notify_waiters();
                    }
                    Err(err) => warn!(%err, "could not parse Niri event"),
                },
                Err(err) => {
                    warn!(%err, "Niri event stream read failed");
                    event_closed.store(true, Ordering::SeqCst);
                    notify.notify_waiters();
                    return;
                }
            }
        }
    }))
}

async fn apply_event_value(value: &serde_json::Value, state: &RwLock<ObservedState>) {
    if let Some(payload) = value.get("WorkspacesChanged") {
        if let Ok(workspaces) = serde_json::from_value::<Vec<Workspace>>(
            payload.get("workspaces").cloned().unwrap_or_default(),
        ) {
            state.write().await.workspaces =
                workspaces.into_iter().map(convert_workspace).collect();
        }
        return;
    }

    if let Some(payload) = value.get("WindowsChanged") {
        if let Ok(windows) = serde_json::from_value::<Vec<Window>>(
            payload.get("windows").cloned().unwrap_or_default(),
        ) {
            state.write().await.windows = windows.into_iter().map(convert_window).collect();
        }
        return;
    }

    if let Some(payload) = value.get("WindowOpenedOrChanged") {
        if let Some(raw) = payload.get("window") {
            if let Ok(window) = serde_json::from_value::<Window>(raw.clone()) {
                let window = convert_window(window);
                let mut state = state.write().await;
                if window.is_focused {
                    for existing in &mut state.windows {
                        existing.is_focused = false;
                    }
                }
                if let Some(existing) = state.windows.iter_mut().find(|w| w.id == window.id) {
                    *existing = window;
                } else {
                    state.windows.push(window);
                }
            }
        }
        return;
    }

    if let Some(payload) = value.get("WindowClosed") {
        if let Some(id) = payload.get("id").and_then(serde_json::Value::as_u64) {
            state.write().await.windows.retain(|window| window.id != id);
        }
        return;
    }

    if let Some(payload) = value.get("WindowFocusChanged") {
        let id = payload.get("id").and_then(serde_json::Value::as_u64);
        for window in &mut state.write().await.windows {
            window.is_focused = Some(window.id) == id;
        }
        return;
    }

    if let Some(payload) = value.get("WindowLayoutsChanged") {
        if let Some(raw) = payload.get("changes") {
            if let Ok(changes) = serde_json::from_value::<Vec<(u64, WindowLayout)>>(raw.clone()) {
                let mut state = state.write().await;
                for (id, layout) in changes {
                    if let Some(window) = state.windows.iter_mut().find(|w| w.id == id) {
                        apply_layout(window, &layout);
                    }
                }
            }
        }
        return;
    }

    if let Some(payload) = value.get("WorkspaceActivated") {
        let Some(id) = payload.get("id").and_then(serde_json::Value::as_u64) else {
            return;
        };
        let focused = payload
            .get("focused")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let mut state = state.write().await;
        let output = state
            .workspaces
            .iter()
            .find(|workspace| workspace.id == id)
            .and_then(|workspace| workspace.output.clone());
        for workspace in &mut state.workspaces {
            if output.is_some() && workspace.output == output {
                workspace.is_active = workspace.id == id;
            }
            if focused {
                workspace.is_focused = workspace.id == id;
            }
        }
    }
}

fn convert_window(window: Window) -> RuntimeWindow {
    let process_exe = window.pid.and_then(process_exe);
    let cwd = window.pid.and_then(effective_process_cwd);
    let (column, tile_index) = window
        .layout
        .pos_in_scrolling_layout
        .map(|(column, tile)| (Some(column), Some(tile)))
        .unwrap_or((None, None));

    RuntimeWindow {
        id: window.id,
        title: window.title,
        app_id: window.app_id,
        pid: window.pid,
        process_exe,
        cwd,
        workspace_id: window.workspace_id,
        is_focused: window.is_focused,
        is_floating: window.is_floating,
        column,
        tile_index,
        tile_width: window.layout.tile_size.0,
        tile_height: window.layout.tile_size.1,
    }
}

fn refresh_process_metadata(window: &mut RuntimeWindow) {
    let Some(pid) = window.pid else {
        return;
    };
    window.process_exe = process_exe(pid).or_else(|| window.process_exe.clone());
    window.cwd = effective_process_cwd(pid).or_else(|| window.cwd.clone());
}

fn niri_wayland_display(socket: &Path) -> Option<String> {
    let file = socket.file_name()?.to_str()?;
    let body = file.strip_prefix("niri.")?.strip_suffix(".sock")?;
    let (display, pid) = body.rsplit_once('.')?;
    pid.parse::<u32>().ok()?;
    (!display.is_empty()).then(|| display.to_owned())
}

fn apply_niri_session_environment(command: &mut Command, socket: &Path) {
    let set_if_missing = |command: &mut Command, key: &str, value: String| {
        if std::env::var_os(key).is_none() && !value.is_empty() {
            command.env(key, value);
        }
    };

    set_if_missing(command, "XDG_CURRENT_DESKTOP", "niri".to_owned());
    set_if_missing(command, "XDG_SESSION_DESKTOP", "niri".to_owned());
    set_if_missing(command, "XDG_SESSION_TYPE", "wayland".to_owned());

    if let Some(display) = niri_wayland_display(socket) {
        set_if_missing(command, "WAYLAND_DISPLAY", display);
    }
    if let Some(runtime) = socket.parent() {
        let runtime = runtime.to_string_lossy().into_owned();
        set_if_missing(command, "XDG_RUNTIME_DIR", runtime.clone());
        let bus = Path::new(&runtime).join("bus");
        if bus.exists() {
            set_if_missing(
                command,
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", bus.display()),
            );
        }
    }
}

fn process_exe(pid: i32) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

fn process_cwd(pid: i32) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/cwd"))
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

fn process_children(pid: i32) -> Vec<i32> {
    std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
        .ok()
        .into_iter()
        .flat_map(|text| {
            text.split_whitespace()
                .filter_map(|value| value.parse::<i32>().ok())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn is_interactive_shell(pid: i32) -> bool {
    let Some(exe) = process_exe(pid) else {
        return false;
    };
    let name = Path::new(&exe)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    matches!(name, "bash" | "fish" | "zsh" | "sh" | "dash" | "nu")
}

fn effective_process_cwd(root: i32) -> Option<String> {
    let fallback = process_cwd(root);
    let mut queue = VecDeque::from([(root, 0usize)]);
    let mut visited = HashSet::new();

    while let Some((pid, depth)) = queue.pop_front() {
        if !visited.insert(pid) {
            continue;
        }
        if pid != root && is_interactive_shell(pid) {
            if let Some(cwd) = process_cwd(pid) {
                return Some(cwd);
            }
        }
        if depth < 3 {
            queue.extend(
                process_children(pid)
                    .into_iter()
                    .map(|child| (child, depth + 1)),
            );
        }
    }

    fallback
}

fn apply_layout(window: &mut RuntimeWindow, layout: &WindowLayout) {
    let (column, tile_index) = layout
        .pos_in_scrolling_layout
        .map(|(column, tile)| (Some(column), Some(tile)))
        .unwrap_or((None, None));
    window.column = column;
    window.tile_index = tile_index;
    window.tile_width = layout.tile_size.0;
    window.tile_height = layout.tile_size.1;
}

fn convert_workspace(workspace: Workspace) -> RuntimeWorkspace {
    RuntimeWorkspace {
        id: workspace.id,
        index: workspace.idx,
        name: workspace.name,
        output: workspace.output,
        is_active: workspace.is_active,
        is_focused: workspace.is_focused,
    }
}

fn convert_outputs(outputs: std::collections::HashMap<String, Output>) -> Vec<OutputInfo> {
    let mut outputs: Vec<_> = outputs
        .into_values()
        .map(|output| OutputInfo {
            name: output.name,
            width: output.logical.map(|logical| logical.width),
            height: output.logical.map(|logical| logical.height),
        })
        .collect();
    outputs.sort_by(|a, b| a.name.cmp(&b.name));
    outputs
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::Path;

    use super::{
        choose_spawn_candidate, focused_output_name, niri_wayland_display,
        resolve_target_output_from_state,
    };
    use workbench_core::{
        CandidateDecision, MatchSpec, ObservedState, OutputFallback, OutputInfo, PlacementSpec,
        Recipe, ReusePolicy, RuntimeWindow, RuntimeWorkspace, WindowSpec,
    };

    fn state() -> ObservedState {
        ObservedState {
            workspaces: vec![RuntimeWorkspace {
                id: 1,
                index: 1,
                name: None,
                output: Some("eDP-1".into()),
                is_active: true,
                is_focused: true,
            }],
            outputs: vec![OutputInfo {
                name: "eDP-1".into(),
                width: Some(1920),
                height: Some(1200),
            }],
            ..ObservedState::default()
        }
    }

    fn recipe(output: Option<&str>, fallback: OutputFallback) -> Recipe {
        Recipe {
            name: None,
            workspace: "Dev".into(),
            output: output.map(str::to_owned),
            output_fallback: fallback,
            focus: None,
            spawn_timeout_ms: 10_000,
            windows: Vec::new(),
        }
    }

    #[test]
    fn focused_output_comes_from_focused_workspace() {
        assert_eq!(focused_output_name(&state()).as_deref(), Some("eDP-1"));
    }

    #[test]
    fn missing_output_falls_back_to_focused_output() {
        let target = resolve_target_output_from_state(
            &recipe(Some("DP-2"), OutputFallback::Focused),
            &state(),
        )
        .unwrap();
        assert_eq!(target.name, "eDP-1");
        assert!(target.fallback_warning.is_some());
    }

    #[test]
    fn missing_output_can_be_strict() {
        let err = resolve_target_output_from_state(
            &recipe(Some("DP-2"), OutputFallback::Error),
            &state(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("DP-2"));
    }

    #[test]
    fn parses_wayland_display_from_niri_socket_name() {
        assert_eq!(
            niri_wayland_display(Path::new("/run/user/1000/niri.wayland-1.1386.sock")).as_deref(),
            Some("wayland-1")
        );
        assert_eq!(niri_wayland_display(Path::new("/tmp/not-niri.sock")), None);
    }

    #[test]
    fn spawned_process_pid_breaks_an_otherwise_ambiguous_tie() {
        let spec = WindowSpec {
            name: "browser".into(),
            command: vec!["browser".into()],
            match_spec: MatchSpec {
                app_id: Some("^browser$".into()),
                ..MatchSpec::default()
            },
            reuse: ReusePolicy::Unique,
            layout: PlacementSpec::default(),
        };
        let make_window = |id, pid| RuntimeWindow {
            id,
            title: Some("same".into()),
            app_id: Some("browser".into()),
            pid: Some(pid),
            process_exe: None,
            cwd: None,
            workspace_id: Some(1),
            is_focused: false,
            is_floating: false,
            column: Some(id as usize),
            tile_index: Some(1),
            tile_width: 800.0,
            tile_height: 600.0,
        };
        let windows = vec![make_window(2, 222), make_window(3, 333)];

        let decision =
            choose_spawn_candidate(&spec, &windows, &HashSet::new(), &HashSet::new(), Some(333))
                .unwrap();

        assert!(matches!(decision, CandidateDecision::One(candidate) if candidate.id == 3));
    }
}
