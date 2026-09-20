use gtk::gdk;
use gtk4 as gtk;

const CSS: &str = r#"
window {
  background: #0f1722;
  color: #edf3fa;
}

.workbench-root {
  background: #0f1722;
}

.workbench-root headerbar {
  background: #101b27;
  color: #edf3fa;
  border-bottom: 1px solid rgba(154, 181, 208, 0.12);
  box-shadow: none;
}

.sidebar {
  min-width: 232px;
  background: #101b27;
  border-right: 1px solid rgba(154, 181, 208, 0.10);
  padding: 14px 10px;
}

.sidebar-button {
  background: transparent;
  border: 0;
  box-shadow: none;
  min-height: 42px;
  border-radius: 9px;
  padding: 0 12px;
  color: #cbd7e4;
}
.sidebar-button:hover {
  background: rgba(137, 166, 194, 0.08);
}
.sidebar-button.nav-active {
  background: rgba(53, 132, 228, 0.16);
  color: #f4f8fc;
  border: 1px solid rgba(83, 148, 224, 0.20);
  box-shadow: inset 3px 0 #3584e4;
}
.sidebar-button.nav-active image {
  color: #6caaf1;
}
.sidebar-nav-label {
  font-size: 15px;
  font-weight: 580;
}

.content {
  padding: 20px 22px 22px 22px;
}

.home-content {
  padding-top: 18px;
}

.title-xl {
  font-size: 27px;
  font-weight: 760;
  letter-spacing: -0.3px;
}

.title-lg {
  font-size: 18px;
  font-weight: 720;
  letter-spacing: -0.1px;
}

.subtitle {
  color: #9fb0c3;
}

.home-subtitle {
  color: #9fb0c3;
  font-size: 14px;
}

.card {
  background: #152332;
  border: 1px solid rgba(151, 180, 208, 0.13);
  border-radius: 12px;
  padding: 15px;
  box-shadow: none;
}

.card:hover {
  background: #192a3a;
  border-color: rgba(160, 190, 219, 0.19);
}

.workbench-card {
  min-height: 90px;
  padding: 14px 16px;
}

.capture-window-row {
  min-height: 72px;
  padding: 12px 14px;
}

.card-selected {
  background: #1a2f45;
  border: 1px solid rgba(70, 143, 227, 0.52);
  border-radius: 12px;
  padding: 15px;
  box-shadow: inset 3px 0 #3584e4;
}

.app-icon-tile {
  min-width: 44px;
  min-height: 44px;
  border-radius: 10px;
  background: #1d2d3d;
  border: 1px solid rgba(157, 185, 213, 0.11);
}

.app-icon-small {
  min-width: 36px;
  min-height: 36px;
  border-radius: 9px;
  background: #1d2d3d;
  border: 1px solid rgba(157, 185, 213, 0.11);
}

.status-pill {
  min-height: 23px;
  border-radius: 999px;
  padding: 1px 9px;
  font-size: 12.5px;
  font-weight: 620;
}

.status-ready {
  color: #91d6a2;
  background: rgba(55, 139, 78, 0.15);
}

.status-warning {
  color: #e5c36a;
  background: rgba(169, 126, 31, 0.14);
}

.status-error {
  color: #e8a093;
  background: rgba(170, 69, 52, 0.14);
}

.status-dot {
  min-width: 8px;
  min-height: 8px;
  border-radius: 999px;
}
.dot-ready { background: #35dc69; }
.dot-warning { background: #f7be32; }
.dot-error { background: #ff775f; }

.action-button {
  min-height: 36px;
  border-radius: 8px;
  padding: 0 12px;
  font-weight: 620;
}

.icon-only-button {
  min-width: 40px;
  min-height: 40px;
  padding: 0;
  border-radius: 9px;
}

.search {
  min-height: 40px;
  border-radius: 10px;
  background: #121f2d;
  border: 1px solid rgba(151, 180, 208, 0.15);
  box-shadow: none;
  padding: 0 10px;
}

.section-title {
  font-size: 16px;
  font-weight: 720;
}

.footer {
  border-top: 1px solid rgba(255, 255, 255, 0.06);
  padding-top: 13px;
  color: #a3a3a3;
}

.shortcut-hint {
  font-size: 12px;
  padding: 2px 6px;
}

.quick-action-hint {
  font-size: 12px;
  font-weight: 600;
}

.status-error-text {
  color: #ff9b8f;
}

.library-row {
  min-height: 76px;
}

.layout-canvas {
  background: #0c1520;
  border: 1px solid rgba(151, 180, 208, 0.12);
  border-radius: 12px;
  padding: 9px;
}

.layout-window {
  background: #172637;
  border: 1px solid rgba(151, 180, 208, 0.13);
  border-radius: 10px;
  padding: 10px;
  box-shadow: none;
}

.layout-window:hover {
  background: #1b2d40;
  border-color: rgba(160, 190, 219, 0.20);
}

.layout-stage {
  border-radius: 10px;
}

.layout-window-selected {
  background: #1a324b;
  border-color: rgba(70, 143, 227, 0.62);
  box-shadow: inset 0 0 0 1px rgba(53, 132, 228, 0.16);
}

.layout-tabbed-window {
  border-top: 3px solid #3584e4;
}

.layout-mode-badge {
  padding: 2px 6px;
  border-radius: 6px;
  background: #1d2d3d;
  color: #a9c7e8;
  font-size: 9px;
  font-weight: 700;
  letter-spacing: 0.4px;
}

.floating-stack {
  min-width: 180px;
}

.floating-layout-window {
  min-height: 46px;
  border-radius: 9px;
  padding: 5px 8px;
  background: #1d2d3d;
  border: 1px solid rgba(83, 148, 224, 0.28);
  box-shadow: none;
  color: #edf3fa;
}

.floating-layout-window:hover {
  background: #22384d;
  border-color: rgba(83, 148, 224, 0.48);
}

.capture-preview {
  min-height: 190px;
  padding: 8px;
}

.capture-preview-window {
  background: #172637;
  border: 1px solid rgba(151, 180, 208, 0.14);
  border-radius: 9px;
  padding: 8px;
}

.capture-preview-label {
  font-size: 12px;
  color: #cbd8e8;
}

.capture-floating-window {
  background: rgba(29, 54, 77, 0.96);
  border: 1px solid rgba(106, 161, 226, 0.38);
  border-radius: 8px;
  padding: 5px 8px;
  color: #dce9f8;
}

.card-selected {
  border-color: #4b94ff;
}

.info-banner {
  background: #13212f;
  border: 1px solid rgba(151, 180, 208, 0.13);
  border-radius: 10px;
  padding: 10px 12px;
  color: #c8d5e3;
}

.field-row {
  min-height: 38px;
}

entry, dropdown, spinbutton {
  border-radius: 8px;
  background: #152332;
  border: 1px solid rgba(151, 180, 208, 0.15);
  box-shadow: none;
}

entry.validation-error {
  border-color: rgba(224, 27, 36, 0.88);
  box-shadow: inset 0 0 0 1px rgba(224, 27, 36, 0.28);
}

entry:focus, dropdown:focus, spinbutton:focus {
  border-color: rgba(78, 145, 220, 0.70);
  box-shadow: 0 0 0 1px rgba(53, 132, 228, 0.16);
}

.language-switch {
  border-radius: 8px;
  background: rgba(126, 149, 172, 0.07);
  border: 1px solid rgba(166, 190, 214, 0.12);
  padding: 2px;
}

.language-button {
  min-width: 38px;
  min-height: 32px;
  padding: 0 8px;
  border: 0;
  border-radius: 7px;
  background: transparent;
  box-shadow: none;
  color: #9fb1c6;
  font-weight: 700;
}

.language-button:hover {
  background: rgba(79, 126, 179, 0.16);
  color: #dbe8f7;
}

.language-button.language-active {
  background: rgba(53, 132, 228, 0.20);
  color: #8bbcf2;
}

.filter-button {
  min-width: 40px;
  min-height: 40px;
  border-radius: 9px;
  padding: 0;
  background: rgba(131, 155, 179, 0.07);
  border: 1px solid rgba(166, 190, 214, 0.12);
  color: #b6c5d5;
  box-shadow: none;
}

.filter-button:hover {
  background: rgba(145, 171, 196, 0.12);
  color: #e0e9f3;
}

.filter-button.filter-active {
  background: rgba(53, 132, 228, 0.17);
  border-color: rgba(80, 149, 228, 0.24);
  color: #7fb5f2;
}

.filter-popover {
  min-width: 190px;
  padding: 10px;
}

.popover-title {
  color: #dce5ef;
  font-weight: 700;
  padding: 2px 4px 4px 4px;
}

.popover-action {
  min-height: 34px;
  border-radius: 7px;
  padding: 0 9px;
  background: transparent;
  border: 0;
  box-shadow: none;
  color: #d0dbe7;
}

.popover-action:hover {
  background: rgba(147, 170, 194, 0.10);
}

.empty-filter {
  min-height: 150px;
  border: 1px dashed rgba(164, 188, 212, 0.14);
  border-radius: 12px;
  padding: 24px;
  color: #9eafc3;
}

.quiet-action {
  background: transparent;
  border-color: transparent;
}

.quiet-action:hover {
  background: rgba(255, 255, 255, 0.06);
  border-color: rgba(255, 255, 255, 0.08);
}

.destructive-action {
  color: #ff9b8f;
}

.destructive-action:hover {
  background: rgba(220, 64, 52, 0.12);
  border-color: rgba(220, 64, 52, 0.18);
}

.repair-action {
  color: #d6c17a;
  background: rgba(151, 118, 37, 0.08);
  border-color: rgba(187, 152, 66, 0.15);
}

.home-primary-action {
  min-width: 102px;
}

.segmented-control {
  border-radius: 8px;
  padding: 2px;
  background: rgba(126, 150, 174, 0.07);
  border: 1px solid rgba(166, 190, 214, 0.12);
}

.segment-button {
  min-height: 32px;
  border-radius: 6px;
  padding: 0 11px;
  background: transparent;
  border: 0;
  box-shadow: none;
  color: #aebdcd;
  font-weight: 600;
}

.segment-button:hover {
  background: rgba(145, 169, 193, 0.08);
  color: #dae5ef;
}

.segment-button:checked {
  background: rgba(53, 132, 228, 0.20);
  color: #8bbcf2;
}

.layout-drop-bar {
  margin-top: 2px;
}

.layout-drop-zone {
  min-height: 36px;
  border-radius: 8px;
  padding: 0 11px;
  border: 1px dashed rgba(154, 180, 206, 0.22);
  background: rgba(119, 145, 169, 0.045);
  color: #91a4b8;
}

.layout-drop-zone:hover {
  background: rgba(119, 145, 169, 0.08);
  border-color: rgba(123, 164, 208, 0.32);
  color: #bfd0e1;
}

.layout-drop-zone:drop(active) {
  background: rgba(53, 132, 228, 0.16);
  border-color: rgba(79, 150, 229, 0.60);
  color: #8dbef5;
}

.layout-drop-label {
  font-size: 12.5px;
  font-weight: 600;
}

.editor-layout-card,
.editor-details-card,
.windows-panel {
  background: #132230;
}

.window-list-row {
  min-height: 64px;
  border-radius: 9px;
  padding: 7px 9px;
  background: transparent;
  border: 1px solid transparent;
  box-shadow: none;
  color: #d4deea;
}

.window-list-row:hover {
  background: rgba(145, 169, 193, 0.08);
  border-color: rgba(170, 194, 217, 0.09);
}

.window-list-selected {
  background: rgba(53, 132, 228, 0.13);
  border-color: rgba(70, 143, 227, 0.24);
  box-shadow: inset 3px 0 #3584e4;
}

.window-picker-row {
  min-height: 58px;
  border-radius: 9px;
  padding: 7px 9px;
  background: #152332;
  border: 1px solid rgba(151, 180, 208, 0.13);
  box-shadow: none;
  color: #edf3fa;
}

.window-picker-row:hover {
  background: #1a2c3d;
  border-color: rgba(160, 190, 219, 0.19);
}

.quick-error {
  border-color: rgba(224, 92, 79, 0.24);
}

.quick-window {
  background: #0f1722;
}

.quick-root {
  padding: 18px;
  background: #0f1722;
}

.quick-root headerbar {
  background: #101b27;
  border-bottom: 1px solid rgba(154, 181, 208, 0.12);
  box-shadow: none;
}

.quick-result {
  min-height: 82px;
  border-radius: 11px;
  background: #152332;
  border: 1px solid rgba(151, 180, 208, 0.13);
  padding: 12px 14px;
}
.quick-result:hover, .quick-result:focus {
  background: #1a2c3d;
  border-color: rgba(70, 143, 227, 0.42);
}

.quick-result.quick-selected {
  background: #1a324b;
  border-color: rgba(70, 143, 227, 0.62);
  box-shadow: inset 3px 0 #3584e4;
}

.header-divider {
  border-bottom: 1px solid rgba(146, 174, 206, 0.18);
}

.dim {
  color: #7f93a8;
}
"#;

pub fn install() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
