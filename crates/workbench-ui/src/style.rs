use gtk::gdk;
use gtk4 as gtk;

const CSS: &str = r#"
window {
  background: #0b121a;
  color: #eef4ff;
}

.workbench-root {
  background: linear-gradient(145deg, #0c151f 0%, #0a1119 72%, #0b131c 100%);
}

.header {
  min-height: 58px;
  background: rgba(15, 28, 41, 0.98);
  border-bottom: 1px solid rgba(146, 174, 206, 0.18);
}

.brand-header {
  min-width: 248px;
  padding: 0 18px;
  border-right: 1px solid rgba(146, 174, 206, 0.14);
}

.top-actions {
  padding: 0 18px;
}

.traffic-dot {
  min-width: 12px;
  min-height: 12px;
  border-radius: 999px;
}
.traffic-red { background: #ff5f57; }
.traffic-yellow { background: #febc2e; }
.traffic-green { background: #28c840; }

.sidebar {
  min-width: 248px;
  background: rgba(13, 27, 40, 0.93);
  border-right: 1px solid rgba(146, 174, 206, 0.14);
  padding: 18px 12px 16px 12px;
}

.sidebar-button {
  background: transparent;
  border: 0;
  box-shadow: none;
  min-height: 46px;
  border-radius: 11px;
  padding: 0 14px;
  color: #d9e4f3;
}
.sidebar-button:hover {
  background: rgba(104, 143, 188, 0.10);
}
.sidebar-button.nav-active {
  background: linear-gradient(90deg, rgba(47, 122, 246, 0.28), rgba(60, 123, 220, 0.18));
  color: #76b1ff;
  border: 1px solid rgba(75, 143, 245, 0.22);
}
.sidebar-button.nav-active image {
  color: #62a8ff;
}
.sidebar-nav-label {
  font-size: 15px;
  font-weight: 580;
}

.content {
  padding: 24px 28px 26px 28px;
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
  color: #9eafc3;
}

.home-subtitle {
  color: #a8bbd1;
  font-size: 14px;
}

.card {
  background: linear-gradient(145deg, rgba(20, 37, 52, 0.96), rgba(16, 30, 43, 0.96));
  border: 1px solid rgba(155, 184, 216, 0.18);
  border-radius: 14px;
  padding: 18px;
  box-shadow: 0 7px 20px rgba(0, 0, 0, 0.12);
}

.card:hover {
  background: linear-gradient(145deg, rgba(23, 43, 61, 0.98), rgba(17, 32, 46, 0.98));
  border-color: rgba(104, 159, 229, 0.34);
}

.workbench-card {
  min-height: 96px;
}

.card-selected {
  background: linear-gradient(145deg, rgba(29, 54, 78, 0.98), rgba(19, 38, 55, 0.98));
  border: 1px solid #4a91ff;
  border-radius: 14px;
  padding: 16px;
  box-shadow: 0 0 0 1px rgba(74, 145, 255, 0.12);
}

.app-icon-tile {
  min-width: 46px;
  min-height: 46px;
  border-radius: 11px;
  background: rgba(49, 70, 91, 0.56);
  border: 1px solid rgba(165, 193, 223, 0.11);
}

.app-icon-small {
  min-width: 38px;
  min-height: 38px;
  border-radius: 9px;
  background: rgba(49, 70, 91, 0.48);
  border: 1px solid rgba(165, 193, 223, 0.10);
}

.status-pill {
  min-height: 24px;
  border-radius: 999px;
  padding: 2px 10px;
  font-size: 13px;
  font-weight: 650;
}

.status-ready {
  color: #8cf1a7;
  background: rgba(35, 160, 73, 0.20);
}

.status-warning {
  color: #ffd46b;
  background: rgba(196, 143, 22, 0.18);
}

.status-error {
  color: #ff9c8b;
  background: rgba(205, 72, 52, 0.18);
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
  min-height: 38px;
  border-radius: 9px;
  padding: 0 14px;
  font-weight: 650;
}

.primary-action {
  background: linear-gradient(180deg, #338bff, #2576ec);
  color: white;
  border: 1px solid rgba(109, 169, 255, 0.46);
  box-shadow: 0 5px 14px rgba(30, 104, 215, 0.26);
}
.primary-action:hover {
  background: linear-gradient(180deg, #4898ff, #2f7ff2);
}

.secondary-action {
  background: rgba(25, 43, 59, 0.95);
  border: 1px solid rgba(165, 190, 216, 0.19);
  color: #dbe7f4;
}
.secondary-action:hover {
  background: rgba(33, 54, 74, 0.98);
  border-color: rgba(165, 195, 229, 0.30);
}

.icon-only-button {
  min-width: 40px;
  min-height: 40px;
  padding: 0;
  border-radius: 9px;
}

.search {
  min-height: 44px;
  border-radius: 10px;
  background: rgba(20, 35, 49, 0.86);
  border: 1px solid rgba(154, 183, 214, 0.16);
  padding: 0 10px;
}

.section-title {
  font-size: 16px;
  font-weight: 720;
}

.muted-panel {
  background: rgba(19, 36, 51, 0.88);
  border: 1px solid rgba(160, 190, 220, 0.16);
  border-radius: 13px;
  padding: 16px;
}

.info-panel-title {
  font-weight: 700;
}

.footer {
  border-top: 1px solid rgba(150, 181, 211, 0.13);
  padding-top: 13px;
  color: #a3b5c9;
}

.layout-canvas {
  background-color: #0d1823;
  background-image: radial-gradient(rgba(117, 156, 199, 0.12) 1px, transparent 1px);
  background-size: 12px 12px;
  border: 1px solid rgba(157, 187, 218, 0.18);
  border-radius: 14px;
  padding: 10px;
}

.layout-window {
  background: linear-gradient(145deg, rgba(26, 47, 65, 0.96), rgba(18, 35, 49, 0.96));
  border: 1px solid rgba(160, 190, 220, 0.20);
  border-radius: 11px;
  padding: 12px;
}

.layout-window:hover {
  border-color: rgba(93, 153, 234, 0.42);
}

.card-selected {
  border-color: #4b94ff;
}

.info-banner {
  background: linear-gradient(90deg, rgba(34, 105, 190, 0.18), rgba(26, 69, 111, 0.11));
  border: 1px solid rgba(74, 146, 234, 0.34);
  border-radius: 10px;
  padding: 12px 14px;
  color: #bdd5f2;
}

.step-badge {
  min-width: 30px;
  min-height: 30px;
  border-radius: 999px;
  background: #388cff;
  color: white;
  font-weight: 750;
}

.field-row {
  min-height: 38px;
}

entry, dropdown, spinbutton {
  border-radius: 8px;
  background: rgba(17, 31, 44, 0.92);
  border: 1px solid rgba(151, 182, 213, 0.19);
}

entry:focus, dropdown:focus, spinbutton:focus {
  border-color: rgba(75, 146, 247, 0.72);
  box-shadow: 0 0 0 1px rgba(75, 146, 247, 0.20);
}

.lang-chip {
  border-radius: 9px;
  min-width: 78px;
  min-height: 38px;
  background: rgba(25, 43, 59, 0.94);
  border: 1px solid rgba(161, 190, 220, 0.17);
}

.quick-window {
  background: rgba(9, 17, 25, 0.98);
}

.quick-root {
  padding: 20px;
  background: linear-gradient(155deg, rgba(13, 28, 42, 0.99), rgba(9, 19, 29, 0.99));
}

.quick-result {
  min-height: 88px;
  border-radius: 12px;
  background: rgba(19, 37, 52, 0.96);
  border: 1px solid rgba(154, 184, 214, 0.17);
  padding: 14px;
}
.quick-result:hover, .quick-result:focus {
  background: rgba(25, 48, 68, 0.98);
  border-color: #3d8cff;
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
