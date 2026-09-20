# niri-workbench

[![CI](https://github.com/t1ktakdev/niri-workbench/actions/workflows/ci.yml/badge.svg)](https://github.com/t1ktakdev/niri-workbench/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/t1ktakdev/niri-workbench)](https://github.com/t1ktakdev/niri-workbench/releases)
[![License](https://img.shields.io/github/license/t1ktakdev/niri-workbench)](LICENSE)

Reproducible project workspaces for the Niri Wayland compositor.

```bash
niri-workbench          # quick graphical launcher
niri-workbench ui       # full manager
niri-workbench doctor
niri-workbench plan rust-dev
niri-workbench open rust-dev
```

`niri-workbench` can snapshot the real windows on your current Niri workspace into a reusable desired-state recipe. Reopening that snapshot reuses matching windows when possible, launches missing applications, restores the named workspace, columns, sizing, floating/tabbed state and final focus.

It is a workspace snapshot/rebuilder, not a byte-for-byte process checkpoint: it restores windows, applications, layout and app-specific launch context where it can, but it does not serialize application memory or claim to preserve every application's internal session state.

## Why

Project workspaces are usually intentional. Arrange the editor, browser and terminals the way you want, choose **Save current**, then reopen that snapshot later. The saved recipe stays editable, so automatic capture and deliberate configuration can coexist.

The core loop is:

```text
recipe
  ↓
observe Niri
  ↓
reuse or spawn windows
  ↓
reconcile workspace / columns / sizes
  ↓
verify observable state
```

Apply is designed to be idempotent. Re-running the same recipe should not create duplicate windows or keep rearranging an already converged workspace.

## Install

Requirements:

- Linux running Niri;
- Niri IPC compatible with `niri-ipc 26.4.0` (the dependency is pinned exactly);
- Rust 1.85+ when building from source.

From source:

```bash
git clone https://github.com/t1ktakdev/niri-workbench.git
cd niri-workbench
cargo build --release
./scripts/install.sh
```

The install script installs both `~/.local/bin/niri-workbench` and `~/.local/bin/niri-workbench-ui`, plus the desktop entry under `~/.local/share/applications` by default. It does not use root or modify Niri configuration.

Release archives are prepared by the tag workflow and contain the binary, license, README, changelog, and install/uninstall scripts.

## Quick start

Create `~/.config/niri-workbench/config.toml`:

```toml
[workbench.rust-dev]
name = "Rust / example"
workspace = "Rust Dev"
focus = "editor"
spawn_timeout_ms = 10000

[[workbench.rust-dev.windows]]
name = "editor"
command = ["code", "~/Projects/example"]

[workbench.rust-dev.windows.match]
app_id = "^code$"
title = "example"

[workbench.rust-dev.windows.layout]
column = 1
column_width = "55%"

[[workbench.rust-dev.windows]]
name = "docs"
command = ["google-chrome-stable", "https://docs.rs"]

[workbench.rust-dev.windows.match]
app_id = "^google-chrome$"
title = "docs.rs"

[workbench.rust-dev.windows.layout]
column = 2
column_width = "30%"
display = "tabbed"

[[workbench.rust-dev.windows]]
name = "terminal"
command = [
  "kitty",
  "--directory",
  "~/Projects/example",
  "bash",
  "-lc",
  "cargo watch -x check; exec bash"
]

[workbench.rust-dev.windows.match]
app_id = "^kitty$"
title = "example|cargo"

[workbench.rust-dev.windows.layout]
column = 3
column_width = "25%"
```

Then either use the GUI or inspect from the CLI before mutating anything:

```bash
niri-workbench ui
niri-workbench doctor
niri-workbench plan rust-dev
niri-workbench apply rust-dev --dry-run
niri-workbench open rust-dev
```

The GUI includes Home, Snapshot, Library, Settings, a visual layout editor, and a compact quick launcher. **Save current** reads the focused Niri workspace and builds a snapshot from the real open windows. It resolves installed applications from desktop entries and Flatpak exports, infers launch context for common apps, and shows the result before saving. Add Window offers real open windows plus applications actually installed on the system. The editor validates the recipe and protects unsaved changes when leaving or closing the window.

Editor shortcuts:

```text
Ctrl+N    add a window from the current Niri workspace
Ctrl+S    save the current recipe
Alt+Left  leave the editor (with an unsaved-changes prompt when needed)
```

The quick launcher supports keyboard selection; `Enter` performs the primary action and `Ctrl+Enter` runs Repair when it is available.

For a Niri key binding, point directly at the quick launcher, for example:

```kdl
Mod+W { spawn "niri-workbench"; }
```

The full manager remains available with `niri-workbench ui`.

## Configuration

A recipe has an optional user-facing `name`, one named Niri `workspace`, and a list of logical windows. Keeping the display name separate from the workspace name lets the UI show labels such as `Rust / niri-workbench` while Niri uses a shorter workspace name such as `Dev`. Each window has:

- a logical `name`;
- an optional spawn `command`;
- a matcher using `window_id`, `app_id`, `title`, `process`, or `pid`;
- a `layout` with column, width/height, display mode, floating state, and optional output.

`app_id`, `title`, and `process` are regular expressions. Match fields are ANDed. A generic matcher that matches multiple existing windows is treated as ambiguous rather than guessed.

`reuse = "never"` can be used when a recipe intentionally requires a fresh window. Otherwise existing unique matches are reused.

Sizes accept percentages or positive pixels:

```toml
column_width = "55%"
column_width = "900px"
window_height = "50%"
```

`~` and environment variables are expanded in command arguments. Commands are executed directly as argv; workbench does not implicitly wrap them in a shell. If shell syntax is needed, put the shell explicitly in the recipe.

For more examples see `examples/`.

### Outputs

A recipe may request an output:

```toml
[workbench.mobile]
workspace = "Android"
output = "DP-2"
output_fallback = "focused"
```

`output_fallback = "focused"` prints a warning and uses the currently focused output when the requested output is disconnected. `"error"` makes that condition fatal.

In v0.1 one logical workbench is one Niri workspace, so its tiling windows cannot be split across multiple outputs.

## Commands

```text
niri-workbench                 # quick launcher
niri-workbench ui              # full GTK manager
niri-workbench quick           # explicit quick launcher
niri-workbench list
niri-workbench show NAME
niri-workbench status NAME
niri-workbench plan NAME
niri-workbench open NAME
niri-workbench repair NAME
niri-workbench apply NAME
niri-workbench apply NAME --dry-run
niri-workbench doctor
```

Use `-v` and `-vv` for diagnostics, or set `RUST_LOG=workbench_niri=debug`.

## How it works

Workbench opens two Niri IPC connections. One is dedicated to EventStream and maintains current window/workspace state; the other sends actions. This follows Niri's IPC model: after a socket enters EventStream mode it no longer accepts normal requests.

Spawning is event-driven. Before launch, workbench records the current window IDs. It then launches the recipe command and waits for matching windows that were not in that baseline. There is no fixed multi-second sleep. If multiple new windows remain plausible, apply times out with candidate details instead of silently picking one.

Reconciliation is staged. Niri processes IPC actions separately, and grouping/moving columns changes later indices, so workbench observes again after each structural mutation rather than executing a stale batch of index-based actions.

The default behavior is non-destructive: workbench does not close windows, kill applications, edit `~/.config/niri/config.kdl`, download code, or require root.

## Limitations

Niri 26.04 exposes window position, tile size, floating state, workspace, app ID, title and PID, but does not expose a column's current display mode. Therefore `display = "tabbed"` or `"normal"` is reasserted during apply and reported as unverifiable; workbench does not pretend it observed that state.

Percentage verification is approximate because IPC exposes logical output dimensions, not the effective working area after layer-shell panels. The requested Niri percentage action is authoritative; workbench avoids retry loops if its approximation differs.

Some column actions are focus-based in Niri. Workbench uses ID-addressable actions where available and restores the configured final focus after reconciliation.

Graphical Snapshot is intentionally conservative. It can infer common launch commands such as Kitty, VS Code, Chrome and Firefox, resolve installed desktop applications, and otherwise fall back to a known process executable when safe. Niri IPC does not expose arbitrary application-internal session state, so the snapshot stays editable instead of pretending restoration is perfect.

For Chrome, Workbench can best-effort recover the detected profile and active page URL. When `sqlite3` is available it may read a temporary local copy of Chrome's History database to map the current window title to its most recent URL; that lookup stays on the machine and is not uploaded. This does **not** preserve a complete tab set, back/forward history, form state, or arbitrary browser memory, and the inferred URL may be unavailable or imperfect.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo build --release
```

Architecture and upstream constraints are documented in `docs/architecture.md`. Live/automated test notes are in `docs/testing.md`, and packaging notes are in `docs/packaging.md`.

## Roadmap

v0.2 adds the snapshot-first GTK workflow, installed-application discovery, safer single-instance navigation, validation/unsaved guards, native window actions, and best-effort application-aware capture on top of the declarative reconciler.

Possible later work: richer capture hints for applications with recoverable document/session metadata, shell completions, stronger desktop integration, and per-recipe environment declarations. No timeline is implied.

## License

MIT. See `LICENSE`.
