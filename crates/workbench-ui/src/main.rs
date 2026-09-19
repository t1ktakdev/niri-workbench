mod i18n;
mod settings;
mod state;
mod style;
mod ui;

use adw::prelude::*;
use libadwaita as adw;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let quick = args.iter().any(|arg| arg == "--quick");
    let start_page = if args.iter().any(|arg| arg == "--capture") {
        Some(ui::StartPage::Capture)
    } else {
        args.iter()
            .find_map(|arg| arg.strip_prefix("--edit="))
            .map(|key| ui::StartPage::Edit(key.to_owned()))
    };

    let app_id = if quick {
        "dev.t1ktak.NiriWorkbench.Quick"
    } else {
        "dev.t1ktak.NiriWorkbench"
    };

    let app = adw::Application::builder().application_id(app_id).build();
    app.connect_activate(move |app| {
        style::install();
        if quick {
            ui::build_quick_launcher(app);
        } else {
            ui::build_main_window(app, start_page.clone());
        }
    });
    app.run_with_args(&["niri-workbench-ui"]);
}
