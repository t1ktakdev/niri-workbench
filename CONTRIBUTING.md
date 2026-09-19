# Contributing

Small, focused changes are welcome.

Before changing behavior, open an issue or discussion if the change affects the
recipe format, matching rules, reconciliation ordering, or supported Niri IPC
version. Those areas have subtle failure modes and are easier to review with a
concrete use case.

## Local checks

Run these before sending a pull request:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --release
```

For planner changes, add a fixture-style test that shows both the observed state
and the expected action. For matching changes, include ambiguous-window cases.
Do not add sleeps to solve EventStream races.

If you test against a live Niri session, use uniquely identifiable test windows
and clean up only windows created by the test.

## Style

Prefer a small direct implementation over a new abstraction. Runtime I/O paths
should return contextual errors instead of panicking. Comments are useful for
ordering constraints, IPC races, or Niri behavior that is not obvious from the
action name.
