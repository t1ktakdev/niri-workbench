# niri-workbench

[![CI](https://github.com/t1ktakdev/niri-workbench/actions/workflows/ci.yml/badge.svg)](https://github.com/t1ktakdev/niri-workbench/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/t1ktakdev/niri-workbench)](https://github.com/t1ktakdev/niri-workbench/releases)
[![License](https://img.shields.io/github/license/t1ktakdev/niri-workbench)](LICENSE)

Reproducible project workspaces for the Niri Wayland compositor.

```bash
niri-workbench doctor
niri-workbench plan rust-dev
niri-workbench apply rust-dev
```

`niri-workbench` turns a declarative project recipe into a real Niri workspace. It reuses matching windows when that is unambiguous, spawns missing applications, moves them to a named workspace, rebuilds columns, applies sizing and tabbed display, then restores a requested final focus.

It is intentionally not a session restore tool. A session manager answers “what was open last time?”; workbench answers “what should this project workspace look like?”

## Why

Project workspaces are usually intentional. A Rust project may need an editor, docs browser, terminal running `cargo watch`, and a second terminal arranged the same way every day. Saving the previous desktop state is a different problem.

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

The install script installs the locally built binary to `~/.local/bin/niri-workbench` by default. It does not use root or modify Niri configuration.

Release archives are prepared by the tag workflow and contain the binary, license, README, changelog, and install/uninstall scripts.

## Quick start

Create `~/.config/niri-workbench/config.toml`:

```toml
[workbench.rust-dev]
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

Then inspect before mutating anything:

```bash
niri-workbench doctor
niri-workbench plan rust-dev
niri-workbench apply rust-dev --dry-run
niri-workbench apply rust-dev
```

## Configuration

A recipe has one named Niri workspace and a list of logical windows. Each window has:

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
niri-workbench list
niri-workbench show NAME
niri-workbench status NAME
niri-workbench plan NAME
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

Capture is intentionally not included in v0.1. IPC cannot reliably infer the correct launch command for an arbitrary GUI window, and current column display mode is not observable.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo build --release
```

Architecture and upstream constraints are documented in `docs/architecture.md`. Live/automated test notes are in `docs/testing.md`, and packaging notes are in `docs/packaging.md`.

## Roadmap

v0.1 focuses on declarative recipes, event-driven matching, idempotent apply, named workspaces, columns, sizing, tabbed display, dry-run and doctor.

Possible later work: safer capture assistance, an interactive recipe generator, shell completions, and per-recipe environment declarations. No timeline is implied.

## License

MIT. See `LICENSE`.
