# Changelog

All notable changes to this project are documented here.

The format is based on Keep a Changelog. The project does not promise a stable
configuration or library API before 1.0.

## [0.2.0] - 2026-09-20

### Added

- Snapshot-first workflow: save the real windows on the current Niri workspace instead of starting from a blank recipe.
- Installed-application discovery from desktop entries and Flatpak exports for real names, icons, launch commands, and app identity.
- Best-effort Chrome profile and active-page restoration when the local profile data is available.
- Native Hide action that keeps the Workbench process and unsaved draft alive.

### Changed

- Reworked the graphical shell around native GTK/libadwaita header bars, actions, preference rows, and a colder graphite/navy visual system.
- Capture now prefers stable app identity over fragile window titles when a title is not needed to disambiguate multiple windows.
- Add Window shows real open windows plus applications actually installed on the system.
- Added live editor status and validation for workspace names, commands, sizes, and regular-expression matchers.
- Added `Ctrl+N`, `Ctrl+S`, and `Alt+Left` editor shortcuts with unsaved-change protection.
- Prevented duplicate manager windows and route repeated launches into the existing application instance.
- Added close/navigation guards for unsaved editor changes and unfinished snapshots.
- Added live snapshot validation and disabled saving while required fields are invalid.
- Matched the desktop entry filename to the Wayland application ID and added Quick/Snapshot desktop actions.

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
