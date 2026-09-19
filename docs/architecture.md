# Architecture notes

This document records the constraints that shaped niri-workbench 0.1.

## Upstream Niri assumptions

The implementation targets the Niri 26.04 IPC shape and pins:

```toml
niri-ipc = "=26.4.0"
```

The exact pin is deliberate. The upstream crate documents that its Rust API follows the Niri version and is not semver-stable across patch releases.

Niri IPC is line-delimited JSON over the Unix socket in `NIRI_SOCKET`. EventStream always sends the current state up front and then state changes. A connection that entered EventStream mode cannot be reused for normal requests, so workbench keeps:

1. one long-lived EventStream connection;
2. one request/action connection.

The state used by workbench is built from:

- `WorkspacesChanged`;
- `WindowsChanged`;
- `WindowOpenedOrChanged`;
- `WindowClosed`;
- `WindowFocusChanged`;
- `WindowLayoutsChanged`;
- `WorkspaceActivated`.

Outputs are refreshed with `Request::Outputs` because output state is not part of the EventStream state used here.

Window IDs are the primary runtime identity. Matchers may also use app ID, title, PID and process executable.

### Runtime actions checked

The 26.04 action surface includes spawn/spawn-sh, focus-window,
focus-workspace, move-window/column-to-workspace, move-window/column-to-monitor,
set-window-width/height, set-column-width, move-column-to-index,
consume/expel window operations, toggle/set-column-display, and
move-window-to-floating/tiling.

Workbench does not shell out to `niri msg` for these. It serializes the
`niri-ipc` request/action types directly. It prefers ID-addressable variants
for window operations. Some column-level actions remain focus-based upstream;
those are isolated in the executor and final focus is restored afterward.

User application commands are launched directly as argv instead of through
Niri's spawn action. That preserves the launched process PID as an optional
matching signal and avoids inventing shell semantics. Shell syntax only runs
when the recipe explicitly names a shell.

For output placement, v0.1 moves the named workspace as a unit rather than
splitting one workbench workspace across monitors. This is why the lower-level
window/column-to-monitor actions are researched but not exposed as recipe
behavior yet.

## Why staged reconciliation

Niri processes IPC requests separately. Time passes between requests, and several layout actions are relative to current focus or current column positions. Moving one column can change the index relevant to the next action.

For that reason, apply does not calculate a long blind list and fire it at the compositor. It uses a bounded loop:

```text
observe
  ↓
build next ReconcilePlan
  ↓
execute one structural mutation
  ↓
wait for EventStream convergence
  ↓
observe again
```

This is intentionally boring. It costs a few extra in-process planning passes, but avoids stale column indices and does not poll Niri.

The broad ordering is:

1. ensure the named workspace exists on the target output;
2. resolve reusable windows;
3. spawn missing windows and resolve them from EventStream;
4. move windows to the workspace;
5. normalize floating/tiling state;
6. repair accidental/incorrect grouping;
7. place columns;
8. consume windows into declared columns;
9. repair tile order inside columns;
10. apply sizes;
11. reassert column display modes;
12. restore final focus;
13. verify observable state.

## Matching

Pre-existing windows are conservative by design. If a recipe says only:

```toml
[workbench.dev.windows.match]
app_id = "^google-chrome$"
```

and two Chrome windows match, workbench refuses to guess.

Signals currently supported:

- exact runtime `window_id`;
- regex `app_id`;
- regex `title`;
- exact `pid`;
- regex process executable path.

All provided fields are ANDed.

After a spawn, the algorithm records a baseline set of existing Niri window IDs before starting the process. Only post-baseline windows are eligible for that spawn. If several new windows satisfy the matcher, an exact PID match with the process just launched can break the tie; otherwise the result stays ambiguous. A short event-driven settle interval catches launchers that create more than one matching toplevel almost back-to-back; the main wait is EventStream-driven and bounded by the recipe timeout.

A command that exits before creating a window is not special-cased as success. Workbench keeps listening until the timeout so wrapper/launcher processes remain valid. Parent-process ancestry is not used as a hard matcher in 0.1 because GUI launchers, containers, and single-instance applications do not preserve it consistently.

## Workspace creation

Niri maintains dynamic empty workspaces. To create a named workbench workspace without editing Niri config, workbench finds an empty workspace on the chosen output and names it through IPC.

If no empty workspace is available, apply fails rather than moving or renaming an occupied workspace.

## Columns and tabbed display

Niri reports `pos_in_scrolling_layout = (column, tile)` and tile/window dimensions. That is enough to reconcile column membership and tile order.

Grouping uses ID-addressable consume/expel actions where available. Column reordering and display changes are still focus-oriented upstream, so workbench temporarily focuses the relevant managed window only when required and restores the configured final focus.

Niri 26.04 does not expose the current column display mode in `WindowLayout`. Therefore display mode cannot participate in a true observed-state equality check. Workbench reasserts the declared `normal`/`tabbed` mode once per apply and clearly reports that it is unverified.

## Sizes

Niri IPC accepts fixed pixels and proportional sizes. Recipes expose:

- `55%`;
- `900`;
- `900px`.

The planner compares pixels directly. Percentage verification is approximate because the compositor action is relative to the effective work area while output IPC exposes logical output dimensions. An apply never loops indefinitely trying to correct that measurement mismatch.

## Crash behavior

There is no daemon state and no persistent apply lock. A crash can leave a partially reconciled workspace, but the next apply starts by observing real Niri state and continues toward the recipe.

Workbench does not close unmanaged windows. If a managed window disappears during apply, the operation fails with its logical name and ID.

## Existing projects reviewed

The design deliberately does not copy session-manager or plugin-daemon architecture.

### niri-session-manager

Automatically saves and restores the desktop/session periodically and maps app IDs to launch commands. Its source of truth is the previously captured session.

niri-workbench instead uses a user-authored project recipe as the source of truth.

### nirinit

Also auto-saves and restores prior window layout, including workspaces, outputs and sizes. It is session lifecycle oriented.

niri-workbench is invoked on demand per named project and is intended to be safely repeatable while the session is already running.

### swaytreesave

Saves and loads compositor tree/layout snapshots and supports Niri among other compositors. It is intentionally multi-compositor and snapshot oriented.

niri-workbench is Niri-first and recipe oriented.

### niri-ror

Provides run-or-raise behavior with useful app ID/title matching. That is a single-window selection/focus problem.

niri-workbench borrows the conservative idea of explicit matching but reconciles a multi-window desired layout.

### niri-scratchpad / niri-scratchpad-rs

Move matching windows into and out of a dedicated stash/scratch workspace. Their lifecycle is “hide/show this window”, not “construct this project scene”.

### Nirius

Provides a daemon plus general utility commands such as focus-or-spawn, move-to-current-workspace and window marks. It is a reusable toolbox rather than a declarative project reconciler.

### piri

A Rust daemon and plugin framework with a unified event distributor and multiple state-driven plugins. This architecture makes sense for continuous automations. Workbench intentionally stays short-lived because apply has a bounded task and does not need a resident plugin host.

### miri

Adds alternate per-workspace tiling layouts such as master-stack. It changes layout behavior rather than reproducing named project scenes.

### awesome-niri

The upstream curated list is useful as the discovery surface for Niri tools and
currently separates window/workspace tools from session managers. It is not a
runtime dependency or an architecture to copy; it is the natural place to
submit niri-workbench after a public release.

## Non-goals for 0.1

- generic Sway/Hyprland/KWin support;
- a permanent daemon;
- destructive session cleanup;
- automatic inference of arbitrary GUI launch commands;
- editing Niri configuration;
- pretending tabbed display is observable when upstream does not expose it.
