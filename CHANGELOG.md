# Changelog

All notable changes to this project are documented here.

The format is based on Keep a Changelog. The project does not promise a stable
configuration or library API before 1.0.

## [0.1.0] - 2026-09-19

### Added

- Declarative TOML workbench recipes.
- Conservative existing-window matching and EventStream-based post-spawn matching.
- Named workspace creation and output fallback handling.
- Idempotent staged reconciliation for workspace placement, columns, tile order,
  sizing, floating state, column display, and final focus.
- `list`, `show`, `status`, `plan`, `open`, `repair`, `apply`, `apply --dry-run`, and `doctor`.
- Native GTK/libadwaita manager with Home, Capture, Library, Settings, visual layout editing, drag-and-drop window placement, preview, and a compact quick launcher.
- Optional user-facing recipe names separate from Niri workspace names.
- Examples for Rust, web, research, and Android development.
- Linux x86_64 release packaging for both CLI and GUI plus an example Arch PKGBUILD.
