mod i18n;
mod settings;
mod state;
mod style;
mod ui;

use std::cell::RefCell;
use std::ffi::OsString;
use std::rc::Rc;

use adw::gio;
use adw::gio::prelude::*;
use adw::prelude::*;
use libadwaita as adw;

fn parse_start_page(args: &[OsString]) -> Option<ui::StartPage> {
    if args.iter().any(|arg| arg == "--capture") {
        Some(ui::StartPage::Capture)
    } else if args.iter().any(|arg| arg == "--library") {
        Some(ui::StartPage::Library)
    } else if args.iter().any(|arg| arg == "--settings") {
        Some(ui::StartPage::Settings)
    } else {
        args.iter()
            .filter_map(|arg| arg.to_str())
            .find_map(|arg| arg.strip_prefix("--edit="))
            .map(|key| ui::StartPage::Edit(key.to_owned()))
    }
}

fn dump_capture(args: &[String]) -> anyhow::Result<()> {
    let snapshot = state::niri_snapshot()?;
    let requested_workspace = args
        .iter()
        .filter_map(|arg| arg.strip_prefix("--workspace="))
        .next();
    let preferred_workspace_id = requested_workspace.and_then(|name| {
        snapshot
            .workspaces
            .iter()
            .find(|workspace| {
                workspace.name.as_deref() == Some(name)
                    || workspace.index.to_string() == name
                    || workspace.id.to_string() == name
            })
            .map(|workspace| workspace.id)
    });
    if requested_workspace.is_some() && preferred_workspace_id.is_none() {
        anyhow::bail!(
            "workspace {:?} was not found",
            requested_workspace.unwrap_or_default()
        );
    }
    let draft = state::capture_workspace(&snapshot, preferred_workspace_id, None)?;
    println!(
        "capture: {} / workspace {} / {} detected window(s)",
        draft.recipe.name.as_deref().unwrap_or("unnamed"),
        draft.recipe.workspace,
        draft.recipe.windows.len()
    );
    for (index, (window, detail)) in draft
        .recipe
        .windows
        .iter()
        .zip(draft.details.iter())
        .enumerate()
    {
        println!(
            "{} {} [{}] app_id={:?} cwd={:?} column={} command={}",
            if detail.include_by_default {
                "INCLUDE"
            } else {
                "EXCLUDE"
            },
            index + 1,
            window.name,
            detail.app_id,
            detail.cwd,
            window.layout.column,
            state::display_command(&window.command)
        );
        if let Some(note) = &detail.inclusion_note {
            println!("  note: {note}");
        }
        println!("  title: {}", detail.title);
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--dump-capture") {
        if let Err(error) = dump_capture(&args) {
            eprintln!("capture error: {error:#}");
            std::process::exit(1);
        }
        return;
    }
    let quick = args.iter().any(|arg| arg == "--quick");
    let app_id = if quick {
        "dev.t1ktak.NiriWorkbench.Quick"
    } else {
        "dev.t1ktak.NiriWorkbench"
    };

    let app = adw::Application::builder()
        .application_id(app_id)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    let pending_start_page = Rc::new(RefCell::new(None::<ui::StartPage>));

    {
        let pending_start_page = Rc::clone(&pending_start_page);
        app.connect_command_line(move |app, command_line| {
            if !quick {
                *pending_start_page.borrow_mut() = parse_start_page(&command_line.arguments());
            }
            app.activate();
            adw::glib::ExitCode::SUCCESS
        });
    }

    app.connect_activate(move |app| {
        if quick {
            if let Some(window) = app.active_window() {
                window.present();
                return;
            }
            style::install();
            ui::build_quick_launcher(app);
            return;
        }

        if app.active_window().is_none() {
            style::install();
        }
        let start_page = pending_start_page.borrow_mut().take();
        ui::present_main_window(app, start_page);
    });

    app.run_with_args(&args);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_capture_route() {
        assert!(matches!(
            parse_start_page(&args(&["niri-workbench-ui", "--capture"])),
            Some(ui::StartPage::Capture)
        ));
    }

    #[test]
    fn capture_route_has_priority() {
        assert!(matches!(
            parse_start_page(&args(&[
                "niri-workbench-ui",
                "--edit=rust-dev",
                "--capture"
            ])),
            Some(ui::StartPage::Capture)
        ));
    }

    #[test]
    fn parses_edit_route() {
        match parse_start_page(&args(&["niri-workbench-ui", "--edit=rust-dev"])) {
            Some(ui::StartPage::Edit(key)) => assert_eq!(key, "rust-dev"),
            other => panic!("unexpected route: {other:?}"),
        }
    }

    #[test]
    fn plain_launch_has_no_forced_route() {
        assert!(parse_start_page(&args(&["niri-workbench-ui"])).is_none());
    }
}
