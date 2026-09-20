# Testing notes

## Automated coverage

The core tests cover configuration parsing and validation, size parsing,
matcher ambiguity and spawn baselines, missing app IDs, output fallback,
planner no-op behavior, grouping, tile ordering, and fixture-based
reconciliation.

The examples are parsed in an integration test so shipped recipes cannot drift
out of sync with the config model.

## Niri 26.04 smoke test

The 0.1.0 candidate was also exercised against a live Niri 26.04 session.

The controlled recipe used two Kitty windows with unique app IDs:

```text
niri-workbench-test-a
niri-workbench-test-b
```

The test verified that apply:

1. created/named an empty workspace without editing Niri config;
2. detected both newly opened windows from EventStream;
3. moved only those windows to the test workspace;
4. grouped them into one column;
5. applied a fixed 700 px column width;
6. set the column to tabbed display;
7. restored the requested final focus;
8. converged on a second apply without spawning duplicates.

After tabbed display was applied, both windows reported the same column and
full column tile height through `niri msg --json windows`.

A negative smoke test intentionally launched without a usable Wayland display.
The application exited without creating a Niri window; workbench waited for the
configured five-second window timeout and reported the spawn command, matcher,
and observed candidates instead of using a fixed sleep or silently succeeding.

All smoke-test windows were closed by their unique test app IDs afterward, the
temporary workspace name was removed, and focus was returned to the original
workspace. No pre-existing user window was closed.

## GTK UI smoke test

The native GTK/libadwaita UI was built in release mode and launched inside the
same live Niri session. Home, Capture, Edit and Quick Launcher were exercised as
real Wayland windows. The UI asks Niri to float and center its own window through
the same IPC library rather than relying on compositor config rules.

A startup race found during Capture testing was fixed by waiting on EventStream
until the UI toplevel with the current process PID is observable, then waiting
for the floating-state event before applying fixed width/height. The current full manager requests a floating 1240x780 window; the Quick Launcher
requests a floating 780x570 window.

## Release gate

Before tagging:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --locked
cargo build --release --locked
```

Also run `niri-workbench doctor` and a read-only `plan` against the target
Niri session.
