use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::str::FromStr;
use std::time::Duration;

use gtk4 as gtk;
use libadwaita as adw;

use adw::prelude::*;
use gtk::glib;
use gtk::glib::value::ToValue;
use workbench_core::{ColumnDisplay, Config, Recipe, ReusePolicy, Size, WindowSpec};

use crate::i18n::{Language, tr};
use crate::settings;
use crate::state::{self, RecipeStatus};

type DropCallback = Rc<dyn Fn(String, String, bool)>;

struct UiState {
    window: adw::ApplicationWindow,
    toast: adw::ToastOverlay,
    shell: gtk::Box,
    stack: RefCell<Option<gtk::Stack>>,
    nav: RefCell<HashMap<String, gtk::Button>>,
    config_path: std::path::PathBuf,
    settings_path: std::path::PathBuf,
    config: RefCell<Config>,
    language: Cell<Language>,
    capture_workspace_id: Cell<Option<u64>>,
    load_error: RefCell<Option<String>>,
}

#[derive(Debug, Clone)]
pub enum StartPage {
    Capture,
    Edit(String),
}

pub fn build_main_window(app: &adw::Application, start_page: Option<StartPage>) {
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);

    let config_path = settings::config_path();
    let settings_path = settings::settings_path();
    let language = settings::load_language(&settings_path);
    let (config, load_error) = match state::load_config_or_empty(&config_path) {
        Ok(config) => (config, None),
        Err(error) => (
            Config {
                workbench: Default::default(),
            },
            Some(error.to_string()),
        ),
    };

    let capture_workspace_id = state::niri_snapshot()
        .ok()
        .and_then(|snapshot| snapshot.focused_workspace().map(|workspace| workspace.id));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Workbench")
        .default_width(1180)
        .default_height(760)
        .build();

    let toast = adw::ToastOverlay::new();
    let shell = gtk::Box::new(gtk::Orientation::Vertical, 0);
    shell.add_css_class("workbench-root");
    toast.set_child(Some(&shell));
    window.set_content(Some(&toast));

    let state = Rc::new(UiState {
        window,
        toast,
        shell,
        stack: RefCell::new(None),
        nav: RefCell::new(HashMap::new()),
        config_path,
        settings_path,
        config: RefCell::new(config),
        language: Cell::new(language),
        capture_workspace_id: Cell::new(capture_workspace_id),
        load_error: RefCell::new(load_error),
    });

    rebuild_shell(&state);
    match start_page {
        Some(StartPage::Capture) => show_capture(&state),
        Some(StartPage::Edit(key)) => show_editor(&state, &key),
        None => {}
    }
    state.window.present();
    state::shape_own_window(1260, 820);

    if let Some(error) = state.load_error.borrow().clone() {
        toast_message(
            &state,
            &format!("{}: {error}", tr(language, "config_error")),
        );
    }
}

fn action_button(icon: &str, label: &str, primary: bool) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("action-button");
    button.add_css_class(if primary {
        "primary-action"
    } else {
        "secondary-action"
    });

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 7);
    row.set_halign(gtk::Align::Center);
    row.set_valign(gtk::Align::Center);

    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(16);
    row.append(&image);

    let text = gtk::Label::new(Some(label));
    row.append(&text);

    button.set_child(Some(&row));
    button
}

fn icon_button(icon: &str, css_class: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("secondary-action");
    button.add_css_class("icon-only-button");
    button.add_css_class(css_class);
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(18);
    button.set_child(Some(&image));
    button
}

fn nav_button(icon: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("sidebar-button");
    button.set_halign(gtk::Align::Fill);
    button.set_hexpand(true);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 13);
    row.set_halign(gtk::Align::Fill);

    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(21);
    row.append(&image);

    let text = gtk::Label::new(Some(label));
    text.set_xalign(0.0);
    text.set_hexpand(true);
    text.add_css_class("sidebar-nav-label");
    row.append(&text);

    button.set_child(Some(&row));
    button
}

fn app_icon_tile(spec: &WindowSpec, size: i32, small: bool) -> gtk::Box {
    let tile = gtk::Box::new(gtk::Orientation::Vertical, 0);
    tile.add_css_class(if small {
        "app-icon-small"
    } else {
        "app-icon-tile"
    });
    tile.set_halign(gtk::Align::Center);
    tile.set_valign(gtk::Align::Center);

    let image = gtk::Image::from_icon_name(state::icon_name(spec));
    image.set_pixel_size(size);
    image.set_halign(gtk::Align::Center);
    image.set_valign(gtk::Align::Center);
    tile.append(&image);
    tile
}

fn status_badge(language: Language, status: &RecipeStatus) -> gtk::Box {
    let (text, class) = status_label(language, status);
    let dot_class = match status {
        RecipeStatus::Ready => "dot-ready",
        RecipeStatus::Missing(_) | RecipeStatus::LayoutChanged => "dot-warning",
        RecipeStatus::Ambiguous | RecipeStatus::Offline => "dot-error",
    };

    let badge = gtk::Box::new(gtk::Orientation::Horizontal, 7);
    badge.add_css_class("status-pill");
    badge.add_css_class(class);
    badge.set_halign(gtk::Align::Start);

    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("status-dot");
    dot.add_css_class(dot_class);
    dot.set_valign(gtk::Align::Center);
    badge.append(&dot);

    let label = gtk::Label::new(Some(&text));
    badge.append(&label);
    badge
}

fn traffic_dot(class: &str) -> gtk::Box {
    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("traffic-dot");
    dot.add_css_class(class);
    dot
}

fn rebuild_shell(state: &Rc<UiState>) {
    clear_box(&state.shell);

    let language = state.language.get();
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    header.add_css_class("header");

    let brand = gtk::Box::new(gtk::Orientation::Horizontal, 9);
    brand.add_css_class("brand-header");
    brand.append(&traffic_dot("traffic-red"));
    brand.append(&traffic_dot("traffic-yellow"));
    brand.append(&traffic_dot("traffic-green"));

    let title = gtk::Label::new(Some("Workbench"));
    title.add_css_class("title-lg");
    title.set_xalign(0.0);
    title.set_margin_start(12);
    brand.append(&title);
    header.append(&brand);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    actions.add_css_class("top-actions");
    actions.set_hexpand(true);
    actions.set_halign(gtk::Align::End);
    actions.set_valign(gtk::Align::Center);

    let lang = gtk::DropDown::from_strings(&["RU", "EN"]);
    lang.add_css_class("lang-chip");
    lang.set_selected(if language == Language::Ru { 0 } else { 1 });
    let weak = Rc::downgrade(state);
    lang.connect_selected_notify(move |dropdown| {
        let Some(state) = weak.upgrade() else {
            return;
        };
        let next = if dropdown.selected() == 0 {
            Language::Ru
        } else {
            Language::En
        };
        if next == state.language.get() {
            return;
        }
        state.language.set(next);
        if let Err(error) = settings::save_language(&state.settings_path, next) {
            toast_message(&state, &error.to_string());
        }
        rebuild_shell(&state);
    });
    actions.append(&lang);

    let new_button = action_button("list-add-symbolic", tr(language, "new"), true);
    let weak = Rc::downgrade(state);
    new_button.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            show_capture(&state);
        }
    });
    actions.append(&new_button);
    header.append(&actions);
    state.shell.append(&header);

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.set_hexpand(true);
    body.set_vexpand(true);

    let sidebar = build_sidebar(state);
    body.append(&sidebar);

    let stack = gtk::Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    *state.stack.borrow_mut() = Some(stack.clone());

    let home = build_home(state);
    stack.add_named(&home, Some("home"));
    stack.set_visible_child_name("home");
    set_nav_active(state, "home");

    body.append(&stack);
    state.shell.append(&body);
}

fn build_sidebar(state: &Rc<UiState>) -> gtk::Box {
    let language = state.language.get();
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 7);
    sidebar.add_css_class("sidebar");
    sidebar.set_size_request(248, -1);

    state.nav.borrow_mut().clear();

    let items = [
        ("home", "go-home-symbolic", tr(language, "home")),
        (
            "capture",
            "view-fullscreen-symbolic",
            tr(language, "capture"),
        ),
        (
            "library",
            "drive-harddisk-symbolic",
            tr(language, "library"),
        ),
        (
            "settings",
            "emblem-system-symbolic",
            tr(language, "settings"),
        ),
    ];

    for (name, icon, label) in items {
        let button = nav_button(icon, label);

        let weak = Rc::downgrade(state);
        let page = name.to_owned();
        button.connect_clicked(move |_| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            match page.as_str() {
                "home" => show_home(&state),
                "capture" => show_capture(&state),
                "library" => show_library(&state),
                "settings" => show_settings(&state),
                _ => {}
            }
        });

        state
            .nav
            .borrow_mut()
            .insert(name.to_owned(), button.clone());
        sidebar.append(&button);
    }

    let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    sidebar.append(&spacer);

    let info = gtk::Box::new(gtk::Orientation::Vertical, 9);
    info.add_css_class("muted-panel");

    let info_heading = gtk::Box::new(gtk::Orientation::Horizontal, 9);
    let bulb = gtk::Image::from_icon_name("dialog-information-symbolic");
    bulb.set_pixel_size(18);
    info_heading.append(&bulb);
    let info_title = gtk::Label::new(Some(tr(language, "what_it_does")));
    info_title.set_xalign(0.0);
    info_title.add_css_class("info-panel-title");
    info_heading.append(&info_title);
    info.append(&info_heading);

    let info_body = gtk::Label::new(Some(tr(language, "what_it_does_body")));
    info_body.set_xalign(0.0);
    info_body.set_wrap(true);
    info_body.set_max_width_chars(24);
    info_body.add_css_class("subtitle");
    info.append(&info_body);

    sidebar.append(&info);
    sidebar
}

fn show_home(state: &Rc<UiState>) {
    replace_page(state, "home", &build_home(state));
    set_nav_active(state, "home");
}

fn show_library(state: &Rc<UiState>) {
    replace_page(state, "library", &build_library(state));
    set_nav_active(state, "library");
}

fn show_settings(state: &Rc<UiState>) {
    replace_page(state, "settings", &build_settings(state));
    set_nav_active(state, "settings");
}

fn replace_page(state: &Rc<UiState>, name: &str, page: &impl IsA<gtk::Widget>) {
    let stack_ref = state.stack.borrow();
    let Some(stack) = stack_ref.as_ref() else {
        return;
    };
    if let Some(old) = stack.child_by_name(name) {
        stack.remove(&old);
    }
    stack.add_named(page, Some(name));
    stack.set_visible_child_name(name);
}

fn set_nav_active(state: &Rc<UiState>, active: &str) {
    for (name, button) in state.nav.borrow().iter() {
        if name == active {
            button.add_css_class("nav-active");
        } else {
            button.remove_css_class("nav-active");
        }
    }
}

fn build_home(state: &Rc<UiState>) -> gtk::ScrolledWindow {
    let language = state.language.get();
    let page = gtk::Box::new(gtk::Orientation::Vertical, 17);
    page.add_css_class("content");

    let heading = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let title = gtk::Label::new(Some("Workbenches"));
    title.add_css_class("title-xl");
    title.set_xalign(0.0);
    heading.append(&title);

    let subtitle = gtk::Label::new(Some(if language == Language::Ru {
        "Сохранённые рабочие окружения. Открывай, исправляй и сразу возвращайся к работе."
    } else {
        "Your saved project workspaces. Launch, repair, and get back to work."
    }));
    subtitle.set_xalign(0.0);
    subtitle.add_css_class("home-subtitle");
    heading.append(&subtitle);
    page.append(&heading);

    let search_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(tr(language, "search")));
    search.add_css_class("search");
    search.set_hexpand(true);
    search_row.append(&search);

    let refresh = icon_button("view-refresh-symbolic", "refresh-button");
    refresh.set_tooltip_text(Some(tr(language, "refresh")));
    let weak = Rc::downgrade(state);
    refresh.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            show_home(&state);
        }
    });
    search_row.append(&refresh);
    page.append(&search_row);

    let list = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let snapshot = state::niri_snapshot().ok();

    let config = state.config.borrow().clone();
    if config.workbench.is_empty() {
        let empty = gtk::Box::new(gtk::Orientation::Vertical, 11);
        empty.add_css_class("card");
        empty.set_valign(gtk::Align::Center);
        empty.set_vexpand(true);
        empty.set_margin_top(16);

        let icon = gtk::Image::from_icon_name("folder-new-symbolic");
        icon.set_pixel_size(52);
        empty.append(&icon);

        let title = gtk::Label::new(Some(tr(language, "empty_title")));
        title.add_css_class("title-lg");
        empty.append(&title);

        let body = gtk::Label::new(Some(tr(language, "empty_body")));
        body.add_css_class("subtitle");
        body.set_wrap(true);
        body.set_justify(gtk::Justification::Center);
        empty.append(&body);

        let capture = action_button("view-fullscreen-symbolic", tr(language, "capture"), true);
        capture.set_halign(gtk::Align::Center);
        let weak = Rc::downgrade(state);
        capture.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                show_capture(&state);
            }
        });
        empty.append(&capture);
        list.append(&empty);
    } else {
        let filter_items: Rc<RefCell<Vec<(String, gtk::Widget)>>> =
            Rc::new(RefCell::new(Vec::new()));

        for (key, recipe) in &config.workbench {
            let status = state::recipe_status(recipe, snapshot.as_ref());
            let card = build_workbench_card(state, key, recipe, status);
            filter_items.borrow_mut().push((
                format!("{key} {} {}", recipe_display_name(recipe), recipe.workspace)
                    .to_ascii_lowercase(),
                card.clone().upcast(),
            ));
            list.append(&card);
        }

        let items = Rc::clone(&filter_items);
        search.connect_search_changed(move |entry| {
            let query = entry.text().to_ascii_lowercase();
            for (haystack, widget) in items.borrow().iter() {
                widget.set_visible(query.is_empty() || haystack.contains(&query));
            }
        });
    }

    page.append(&list);

    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 9);
    footer.add_css_class("footer");
    let info = gtk::Image::from_icon_name("dialog-information-symbolic");
    info.set_pixel_size(17);
    footer.append(&info);

    let hint = gtk::Label::new(Some(tr(language, "open_hint")));
    hint.set_xalign(0.0);
    hint.set_hexpand(true);
    hint.add_css_class("subtitle");
    footer.append(&hint);

    let count = gtk::Label::new(Some(&if language == Language::Ru {
        format!("{} окруж.", config.workbench.len())
    } else {
        format!(
            "{} workbench{}",
            config.workbench.len(),
            if config.workbench.len() == 1 {
                ""
            } else {
                "es"
            }
        )
    }));
    count.add_css_class("subtitle");
    footer.append(&count);
    page.append(&footer);

    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&page)
        .build()
}

fn build_workbench_card(
    state: &Rc<UiState>,
    key: &str,
    recipe: &Recipe,
    status: RecipeStatus,
) -> gtk::Box {
    let language = state.language.get();
    let card = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    card.add_css_class("card");
    card.add_css_class("workbench-card");

    let icons = gtk::Box::new(gtk::Orientation::Horizontal, 7);
    icons.set_valign(gtk::Align::Center);
    for spec in recipe.windows.iter().take(4) {
        icons.append(&app_icon_tile(spec, 31, false));
    }
    card.append(&icons);

    let text = gtk::Box::new(gtk::Orientation::Vertical, 5);
    text.set_hexpand(true);
    text.set_valign(gtk::Align::Center);

    let title = gtk::Label::new(Some(recipe_display_name(recipe)));
    title.set_xalign(0.0);
    title.add_css_class("title-lg");
    text.append(&title);

    let subtitle_text = recipe
        .windows
        .iter()
        .map(friendly_window_name)
        .collect::<Vec<_>>()
        .join(" · ");
    let subtitle = gtk::Label::new(Some(&subtitle_text));
    subtitle.set_xalign(0.0);
    subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
    subtitle.add_css_class("subtitle");
    text.append(&subtitle);
    text.append(&status_badge(language, &status));

    card.append(&text);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_valign(gtk::Align::Center);

    match status {
        RecipeStatus::LayoutChanged => {
            let repair = action_button("wrench-symbolic", tr(language, "repair"), false);
            let weak = Rc::downgrade(state);
            let key_owned = key.to_owned();
            repair.connect_clicked(move |_| {
                let Some(state) = weak.upgrade() else {
                    return;
                };
                match state::launch_cli("repair", &key_owned) {
                    Ok(()) => toast_message(&state, tr(state.language.get(), "repairing")),
                    Err(error) => toast_message(&state, &error.to_string()),
                }
            });
            actions.append(&repair);
        }
        RecipeStatus::Ambiguous | RecipeStatus::Offline => {}
        RecipeStatus::Ready | RecipeStatus::Missing(_) => {
            let open = action_button("media-playback-start-symbolic", tr(language, "open"), true);
            let weak = Rc::downgrade(state);
            let key_owned = key.to_owned();
            open.connect_clicked(move |_| {
                let Some(state) = weak.upgrade() else {
                    return;
                };
                match state::launch_cli("open", &key_owned) {
                    Ok(()) => {
                        toast_message(&state, tr(state.language.get(), "opening"));
                        let weak = Rc::downgrade(&state);
                        glib::timeout_add_local_once(Duration::from_millis(1400), move || {
                            if let Some(state) = weak.upgrade() {
                                show_home(&state);
                            }
                        });
                    }
                    Err(error) => toast_message(&state, &error.to_string()),
                }
            });
            actions.append(&open);
        }
    }

    let edit = action_button("document-edit-symbolic", tr(language, "edit"), false);
    let weak = Rc::downgrade(state);
    let key_owned = key.to_owned();
    edit.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            show_editor(&state, &key_owned);
        }
    });
    actions.append(&edit);

    let more = icon_button("view-more-symbolic", "more-button");
    more.set_tooltip_text(Some(if language == Language::Ru {
        "Дополнительные действия"
    } else {
        "More actions"
    }));
    let weak = Rc::downgrade(state);
    let key_owned = key.to_owned();
    more.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            show_editor(&state, &key_owned);
        }
    });
    actions.append(&more);

    card.append(&actions);
    card
}

fn build_library(state: &Rc<UiState>) -> gtk::ScrolledWindow {
    let language = state.language.get();
    let page = gtk::Box::new(gtk::Orientation::Vertical, 14);
    page.add_css_class("content");

    let title = gtk::Label::new(Some(tr(language, "library")));
    title.set_xalign(0.0);
    title.add_css_class("title-xl");
    page.append(&title);

    let subtitle = gtk::Label::new(Some(if language == Language::Ru {
        "Все сохранённые окружения. Открой окружение или перейди в визуальный редактор."
    } else {
        "All saved workbenches. Open one or jump into the visual editor."
    }));
    subtitle.set_xalign(0.0);
    subtitle.add_css_class("subtitle");
    page.append(&subtitle);

    let config = state.config.borrow().clone();
    for (key, recipe) in &config.workbench {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("card");

        let icon = gtk::Image::from_icon_name("folder-symbolic");
        icon.set_pixel_size(28);
        row.append(&icon);

        let labels = gtk::Box::new(gtk::Orientation::Vertical, 3);
        labels.set_hexpand(true);
        let name = gtk::Label::new(Some(recipe_display_name(recipe)));
        name.set_xalign(0.0);
        name.add_css_class("title-lg");
        labels.append(&name);
        let detail = gtk::Label::new(Some(&format!(
            "{key} · {} {}",
            recipe.windows.len(),
            if language == Language::Ru {
                "окон"
            } else {
                "windows"
            }
        )));
        detail.set_xalign(0.0);
        detail.add_css_class("subtitle");
        labels.append(&detail);
        row.append(&labels);

        let edit = gtk::Button::with_label(tr(language, "edit"));
        edit.add_css_class("secondary-action");
        let weak = Rc::downgrade(state);
        let key_owned = key.clone();
        edit.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                show_editor(&state, &key_owned);
            }
        });
        row.append(&edit);

        page.append(&row);
    }

    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&page)
        .build()
}

fn build_settings(state: &Rc<UiState>) -> gtk::ScrolledWindow {
    let language = state.language.get();
    let page = gtk::Box::new(gtk::Orientation::Vertical, 16);
    page.add_css_class("content");

    let title = gtk::Label::new(Some(tr(language, "settings")));
    title.add_css_class("title-xl");
    title.set_xalign(0.0);
    page.append(&title);

    let card = gtk::Box::new(gtk::Orientation::Vertical, 12);
    card.add_css_class("card");

    let section = gtk::Label::new(Some(tr(language, "appearance")));
    section.add_css_class("section-title");
    section.set_xalign(0.0);
    card.append(&section);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let labels = gtk::Box::new(gtk::Orientation::Vertical, 4);
    labels.set_hexpand(true);
    let label = gtk::Label::new(Some(tr(language, "language")));
    label.set_xalign(0.0);
    labels.append(&label);
    let help = gtk::Label::new(Some(tr(language, "language_help")));
    help.set_xalign(0.0);
    help.set_wrap(true);
    help.add_css_class("subtitle");
    labels.append(&help);
    row.append(&labels);

    let dropdown = gtk::DropDown::from_strings(&["Русский", "English"]);
    dropdown.set_selected(if language == Language::Ru { 0 } else { 1 });
    let weak = Rc::downgrade(state);
    dropdown.connect_selected_notify(move |dropdown| {
        let Some(state) = weak.upgrade() else {
            return;
        };
        let next = if dropdown.selected() == 0 {
            Language::Ru
        } else {
            Language::En
        };
        if next != state.language.get() {
            state.language.set(next);
            if let Err(error) = settings::save_language(&state.settings_path, next) {
                toast_message(&state, &error.to_string());
            }
            rebuild_shell(&state);
            show_settings(&state);
        }
    });
    row.append(&dropdown);
    card.append(&row);

    let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
    card.append(&separator);

    let config_row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let path_title = gtk::Label::new(Some(if language == Language::Ru {
        "Файл конфигурации"
    } else {
        "Configuration file"
    }));
    path_title.set_xalign(0.0);
    config_row.append(&path_title);
    let path = gtk::Label::new(Some(&state.config_path.display().to_string()));
    path.set_xalign(0.0);
    path.set_selectable(true);
    path.add_css_class("subtitle");
    config_row.append(&path);
    card.append(&config_row);

    page.append(&card);

    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&page)
        .build()
}

fn status_label(language: Language, status: &RecipeStatus) -> (String, &'static str) {
    match status {
        RecipeStatus::Ready => (tr(language, "ready").to_owned(), "status-ready"),
        RecipeStatus::Missing(1) => (tr(language, "missing_one").to_owned(), "status-warning"),
        RecipeStatus::Missing(count) => (
            format!("{count} {}", tr(language, "missing_many")),
            "status-warning",
        ),
        RecipeStatus::LayoutChanged => {
            (tr(language, "layout_changed").to_owned(), "status-warning")
        }
        RecipeStatus::Ambiguous => (tr(language, "ambiguous").to_owned(), "status-error"),
        RecipeStatus::Offline => (tr(language, "offline").to_owned(), "status-error"),
    }
}

fn recipe_display_name(recipe: &Recipe) -> &str {
    recipe.name.as_deref().unwrap_or(&recipe.workspace)
}

fn friendly_window_name(spec: &WindowSpec) -> String {
    let app = spec
        .match_spec
        .app_id
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if app.contains("code") {
        "VS Code".to_owned()
    } else if app.contains("kitty") {
        "Kitty".to_owned()
    } else if app.contains("chrome") {
        "Chrome".to_owned()
    } else if app.contains("firefox") {
        "Firefox".to_owned()
    } else if app.contains("android") || app.contains("studio") {
        "Android Studio".to_owned()
    } else {
        spec.name.clone()
    }
}

fn capture_behavior_text(spec: &WindowSpec, language: Language) -> &'static str {
    let app = spec
        .match_spec
        .app_id
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if app.contains("code") {
        if language == Language::Ru {
            "Переоткрыть этот проект"
        } else {
            "Reopen this project"
        }
    } else if app.contains("kitty") || app.contains("terminal") {
        if language == Language::Ru {
            "Открыть терминал в этой папке"
        } else {
            "Open terminal in this directory"
        }
    } else if app.contains("chrome") || app.contains("firefox") || app.contains("browser") {
        if language == Language::Ru {
            "Открыть браузер"
        } else {
            "Open browser"
        }
    } else if language == Language::Ru {
        "Запустить приложение"
    } else {
        "Launch application"
    }
}

fn toast_message(state: &Rc<UiState>, message: &str) {
    state.toast.add_toast(adw::Toast::new(message));
}

fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn show_capture(state: &Rc<UiState>) {
    replace_page(state, "capture", &build_capture(state));
    set_nav_active(state, "capture");
}

fn build_capture(state: &Rc<UiState>) -> gtk::ScrolledWindow {
    let language = state.language.get();
    let snapshot = match state::niri_snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return error_page(
                state,
                tr(language, "capture_error"),
                &error.to_string(),
                Some("capture"),
            );
        }
    };
    let draft = match state::capture_workspace(&snapshot, state.capture_workspace_id.get()) {
        Ok(draft) => draft,
        Err(error) => {
            return error_page(
                state,
                tr(language, "capture_error"),
                &error.to_string(),
                Some("capture"),
            );
        }
    };
    let draft = Rc::new(RefCell::new(draft));

    let page = gtk::Box::new(gtk::Orientation::Vertical, 16);
    page.add_css_class("content");

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let title = gtk::Label::new(Some(tr(language, "capture_title")));
    title.add_css_class("title-xl");
    title.set_xalign(0.0);
    title.set_hexpand(true);
    header.append(&title);

    let cancel = action_button("window-close-symbolic", tr(language, "cancel"), false);
    let weak = Rc::downgrade(state);
    cancel.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            show_home(&state);
        }
    });
    header.append(&cancel);

    let create = action_button(
        "document-save-symbolic",
        tr(language, "create_workbench"),
        true,
    );
    header.append(&create);
    page.append(&header);

    let banner = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    banner.add_css_class("info-banner");
    let banner_icon = gtk::Image::from_icon_name("camera-photo-symbolic");
    banner_icon.set_pixel_size(19);
    banner.append(&banner_icon);
    let banner_text = gtk::Label::new(Some(tr(language, "capture_intro")));
    banner_text.set_xalign(0.0);
    banner_text.set_wrap(true);
    banner_text.set_hexpand(true);
    banner.append(&banner_text);
    page.append(&banner);

    let detected_header = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let step = gtk::Label::new(Some("1"));
    step.add_css_class("step-badge");
    detected_header.append(&step);
    let detected_title = gtk::Label::new(Some(&format!(
        "{} ({})",
        tr(language, "detected_windows"),
        draft.borrow().recipe.windows.len()
    )));
    detected_title.add_css_class("section-title");
    detected_title.set_xalign(0.0);
    detected_title.set_hexpand(true);
    detected_header.append(&detected_title);
    let detected_help = gtk::Label::new(Some(if language == Language::Ru {
        "Проверь, как Workbench будет открывать каждое приложение."
    } else {
        "Review how each app should be launched."
    }));
    detected_help.add_css_class("subtitle");
    detected_header.append(&detected_help);
    page.append(&detected_header);

    let command_entries: Rc<RefCell<Vec<(usize, gtk::Entry)>>> = Rc::new(RefCell::new(Vec::new()));
    let windows_box = gtk::Box::new(gtk::Orientation::Vertical, 10);

    {
        let borrowed = draft.borrow();
        for (index, spec) in borrowed.recipe.windows.iter().enumerate() {
            let detail = &borrowed.details[index];
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 13);
            row.add_css_class("card");

            row.append(&app_icon_tile(spec, 31, false));

            let labels = gtk::Box::new(gtk::Orientation::Vertical, 4);
            labels.set_width_request(280);
            let title = gtk::Label::new(Some(&detail.title));
            title.set_xalign(0.0);
            title.set_ellipsize(gtk::pango::EllipsizeMode::End);
            title.add_css_class("title-lg");
            labels.append(&title);

            let mut meta = detail.app_id.clone();
            if let Some(cwd) = &detail.cwd {
                if !meta.is_empty() {
                    meta.push_str(" · ");
                }
                meta.push_str(cwd);
            }
            let meta_label = gtk::Label::new(Some(&meta));
            meta_label.set_xalign(0.0);
            meta_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            meta_label.add_css_class("subtitle");
            labels.append(&meta_label);
            row.append(&labels);

            let launch = gtk::Box::new(gtk::Orientation::Vertical, 5);
            launch.set_hexpand(true);
            let behavior = gtk::Label::new(Some(capture_behavior_text(spec, language)));
            behavior.set_xalign(0.0);
            behavior.add_css_class("subtitle");
            launch.append(&behavior);

            let command = gtk::Entry::new();
            command.set_hexpand(true);
            let command_text = state::display_command(&spec.command);
            command.set_text(&command_text);
            command.set_placeholder_text(Some(tr(language, "reuse_only")));
            command.set_tooltip_text(Some(tr(language, "launch_command")));
            launch.append(&command);
            row.append(&launch);

            let more = icon_button("view-more-symbolic", "more-button");
            more.set_valign(gtk::Align::Center);
            more.set_tooltip_text(Some(tr(language, "advanced")));
            row.append(&more);

            command_entries.borrow_mut().push((index, command));
            windows_box.append(&row);
        }
    }
    page.append(&windows_box);

    let lower = gtk::Box::new(gtk::Orientation::Horizontal, 14);

    let details_card = gtk::Box::new(gtk::Orientation::Vertical, 11);
    details_card.add_css_class("card");
    details_card.set_hexpand(true);

    let details_header = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let step = gtk::Label::new(Some("2"));
    step.add_css_class("step-badge");
    details_header.append(&step);
    let section = gtk::Label::new(Some(tr(language, "workbench_details")));
    section.add_css_class("section-title");
    details_header.append(&section);
    details_card.append(&details_header);

    let name_entry = gtk::Entry::new();
    name_entry.set_text(recipe_display_name(&draft.borrow().recipe));
    details_card.append(&labeled_control(tr(language, "name"), &name_entry));

    let workspace_entry = gtk::Entry::new();
    workspace_entry.set_text(&draft.borrow().recipe.workspace);
    details_card.append(&labeled_control(
        tr(language, "workspace_name"),
        &workspace_entry,
    ));

    let output_value = draft
        .borrow()
        .recipe
        .output
        .clone()
        .unwrap_or_else(|| tr(language, "auto").to_owned());
    let output = gtk::Button::with_label(&output_value);
    output.add_css_class("secondary-action");
    output.set_sensitive(false);
    details_card.append(&labeled_control(tr(language, "output"), &output));

    let advanced_note = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let info = gtk::Image::from_icon_name("dialog-information-symbolic");
    info.set_pixel_size(16);
    advanced_note.append(&info);
    let note = gtk::Label::new(Some(if language == Language::Ru {
        "Точные правила сопоставления можно изменить позже."
    } else {
        "Advanced matching rules can be edited later."
    }));
    note.set_xalign(0.0);
    note.set_wrap(true);
    note.add_css_class("subtitle");
    advanced_note.append(&note);
    details_card.append(&advanced_note);
    lower.append(&details_card);

    let preview_card = gtk::Box::new(gtk::Orientation::Vertical, 10);
    preview_card.add_css_class("card");
    preview_card.set_hexpand(true);

    let preview_header = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let step = gtk::Label::new(Some("3"));
    step.add_css_class("step-badge");
    preview_header.append(&step);
    let preview_title = gtk::Label::new(Some(if language == Language::Ru {
        "Предпросмотр макета"
    } else {
        "Layout preview"
    }));
    preview_title.add_css_class("section-title");
    preview_title.set_hexpand(true);
    preview_title.set_xalign(0.0);
    preview_header.append(&preview_title);
    let detected_count = gtk::Label::new(Some(&format!(
        "{} {}",
        draft.borrow().recipe.windows.len(),
        if language == Language::Ru {
            "окон"
        } else {
            "windows"
        }
    )));
    detected_count.add_css_class("subtitle");
    preview_header.append(&detected_count);
    preview_card.append(&preview_header);
    preview_card.append(&build_layout_canvas(
        &draft.borrow().recipe,
        None,
        None,
        None,
    ));
    lower.append(&preview_card);

    page.append(&lower);

    let weak = Rc::downgrade(state);
    let draft_for_save = Rc::clone(&draft);
    let entries_for_save = Rc::clone(&command_entries);
    let name_for_save = name_entry.clone();
    let workspace_for_save = workspace_entry.clone();
    create.connect_clicked(move |_| {
        let Some(state) = weak.upgrade() else {
            return;
        };

        let mut recipe = draft_for_save.borrow().recipe.clone();
        let workspace_name = workspace_for_save.text().trim().to_owned();
        if workspace_name.is_empty() {
            toast_message(&state, "Workspace name cannot be empty");
            return;
        }
        recipe.workspace = workspace_name;

        for (index, entry) in entries_for_save.borrow().iter() {
            match state::parse_command(entry.text().as_str()) {
                Ok(command) => recipe.windows[*index].command = command,
                Err(error) => {
                    toast_message(&state, &error.to_string());
                    return;
                }
            }
        }

        let display_name = name_for_save.text().trim().to_owned();
        recipe.name = (!display_name.is_empty()).then_some(display_name.clone());
        let base = state::unique_slug(if display_name.is_empty() {
            &recipe.workspace
        } else {
            &display_name
        });
        let base = if base.is_empty() {
            "workbench".to_owned()
        } else {
            base
        };

        let mut next = state.config.borrow().clone();
        let mut key = base.clone();
        let mut suffix = 2usize;
        while next.workbench.contains_key(&key) {
            key = format!("{base}-{suffix}");
            suffix += 1;
        }
        next.workbench.insert(key, recipe);

        match state::persist_config(&state.config_path, &next) {
            Ok(()) => {
                *state.config.borrow_mut() = next;
                toast_message(&state, tr(state.language.get(), "saved"));
                show_home(&state);
            }
            Err(error) => toast_message(&state, &error.to_string()),
        }
    });

    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&page)
        .build()
}

fn build_layout_canvas(
    recipe: &Recipe,
    selected: Option<&str>,
    on_select: Option<Rc<dyn Fn(String)>>,
    on_drop: Option<DropCallback>,
) -> gtk::Box {
    let canvas = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    canvas.add_css_class("layout-canvas");
    canvas.set_height_request(300);

    let mut columns: BTreeMap<usize, Vec<&WindowSpec>> = BTreeMap::new();
    let mut floating = Vec::new();
    for spec in &recipe.windows {
        if spec.layout.floating {
            floating.push(spec);
        } else {
            columns.entry(spec.layout.column).or_default().push(spec);
        }
    }

    let default_fraction = if columns.is_empty() {
        1.0
    } else {
        1.0 / columns.len() as f64
    };

    for windows in columns.values() {
        let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let fraction = windows
            .iter()
            .find_map(|window| match window.layout.column_width {
                Some(Size::Percent(value)) => Some(value),
                _ => None,
            })
            .unwrap_or(default_fraction);
        column.set_width_request((680.0 * fraction).round().clamp(150.0, 520.0) as i32);
        column.set_hexpand(true);

        let default_height = if windows.is_empty() {
            1.0
        } else {
            1.0 / windows.len() as f64
        };

        for spec in windows {
            let button = gtk::Button::new();
            button.add_css_class(if selected == Some(spec.name.as_str()) {
                "card-selected"
            } else {
                "layout-window"
            });
            let fraction = match spec.layout.window_height {
                Some(Size::Percent(value)) => value,
                _ => default_height,
            };
            button.set_height_request((255.0 * fraction).round().clamp(90.0, 250.0) as i32);
            button.set_vexpand(true);

            let overlay = gtk::Overlay::new();
            let inside = gtk::Box::new(gtk::Orientation::Vertical, 6);
            inside.set_valign(gtk::Align::Center);
            inside.set_halign(gtk::Align::Center);

            let icon = gtk::Image::from_icon_name(state::icon_name(spec));
            icon.set_pixel_size(38);
            inside.append(&icon);

            let title = gtk::Label::new(Some(&friendly_window_name(spec)));
            title.add_css_class("section-title");
            inside.append(&title);

            let logical = gtk::Label::new(Some(&spec.name));
            logical.add_css_class("subtitle");
            inside.append(&logical);
            overlay.set_child(Some(&inside));

            let handle = gtk::Image::from_icon_name("list-drag-handle-symbolic");
            handle.set_pixel_size(14);
            handle.set_halign(gtk::Align::Start);
            handle.set_valign(gtk::Align::Start);
            handle.set_margin_top(6);
            handle.set_margin_start(6);
            handle.add_css_class("dim");
            overlay.add_overlay(&handle);

            let menu = gtk::Image::from_icon_name("view-more-symbolic");
            menu.set_pixel_size(14);
            menu.set_halign(gtk::Align::End);
            menu.set_valign(gtk::Align::Start);
            menu.set_margin_top(6);
            menu.set_margin_end(6);
            menu.add_css_class("dim");
            overlay.add_overlay(&menu);

            button.set_child(Some(&overlay));

            if let Some(callback) = on_select.clone() {
                let name = spec.name.clone();
                button.connect_clicked(move |_| callback(name.clone()));
            }

            if on_drop.is_some() {
                let source = gtk::DragSource::new();
                source.set_actions(gtk::gdk::DragAction::MOVE);
                let drag_name = spec.name.clone();
                source.connect_prepare(move |_, _, _| {
                    Some(gtk::gdk::ContentProvider::for_value(&drag_name.to_value()))
                });
                button.add_controller(source);

                let target = gtk::DropTarget::new(glib::Type::STRING, gtk::gdk::DragAction::MOVE);
                if let Some(callback) = on_drop.clone() {
                    let target_name = spec.name.clone();
                    let button_for_drop = button.clone();
                    target.connect_drop(move |_, value, _, y| {
                        let Ok(dragged) = value.get::<String>() else {
                            return false;
                        };
                        if dragged == target_name {
                            return false;
                        }

                        let height = f64::from(button_for_drop.height().max(1));
                        callback(dragged, target_name.clone(), y >= height / 2.0);
                        true
                    });
                }
                button.add_controller(target);
            }

            column.append(&button);
        }

        canvas.append(&column);
    }

    for spec in floating {
        let button = gtk::Button::new();
        button.add_css_class("layout-window");

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.append(&app_icon_tile(spec, 25, true));
        let label = gtk::Label::new(Some(&format!("◫ {}", friendly_window_name(spec))));
        row.append(&label);
        button.set_child(Some(&row));

        if let Some(callback) = on_select.clone() {
            let name = spec.name.clone();
            button.connect_clicked(move |_| callback(name.clone()));
        }
        canvas.append(&button);
    }

    canvas
}

fn show_editor(state: &Rc<UiState>, key: &str) {
    let Some(recipe) = state.config.borrow().workbench.get(key).cloned() else {
        toast_message(state, "Unknown workbench");
        return;
    };
    let selected = recipe.windows.first().map(|window| window.name.clone());
    let session = Rc::new(RefCell::new(recipe));
    show_editor_session(state, key, session, selected);
}

fn show_editor_session(
    state: &Rc<UiState>,
    key: &str,
    draft: Rc<RefCell<Recipe>>,
    selected: Option<String>,
) {
    let page = build_editor_page(state, key, Rc::clone(&draft), selected);
    replace_page(state, "editor", &page);
    set_nav_active(state, "library");
}

fn build_editor_page(
    state: &Rc<UiState>,
    key: &str,
    draft: Rc<RefCell<Recipe>>,
    selected: Option<String>,
) -> gtk::ScrolledWindow {
    let language = state.language.get();
    let page = gtk::Box::new(gtk::Orientation::Vertical, 14);
    page.add_css_class("content");

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let back = icon_button("go-previous-symbolic", "back-button");
    let weak = Rc::downgrade(state);
    back.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            show_library(&state);
        }
    });
    header.append(&back);

    let heading = gtk::Box::new(gtk::Orientation::Vertical, 2);
    heading.set_hexpand(true);
    let title = gtk::Label::new(Some(tr(language, "edit_workbench")));
    title.add_css_class("title-xl");
    title.set_xalign(0.0);
    heading.append(&title);
    let subtitle = {
        let borrowed = draft.borrow();
        gtk::Label::new(Some(&format!(
            "{} / {}",
            recipe_display_name(&borrowed),
            borrowed.workspace
        )))
    };
    subtitle.set_xalign(0.0);
    subtitle.add_css_class("subtitle");
    heading.append(&subtitle);
    header.append(&heading);

    let preview = action_button("view-reveal-symbolic", tr(language, "preview"), false);
    {
        let state = Rc::clone(state);
        let draft = Rc::clone(&draft);
        preview.connect_clicked(move |_| {
            show_layout_preview(&state, &draft.borrow());
        });
    }
    header.append(&preview);

    let cancel = action_button("window-close-symbolic", tr(language, "cancel"), false);
    let weak = Rc::downgrade(state);
    cancel.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            show_library(&state);
        }
    });
    header.append(&cancel);

    let save = action_button("object-select-symbolic", tr(language, "save"), true);
    header.append(&save);
    page.append(&header);

    let main = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    main.set_hexpand(true);

    let left = gtk::Box::new(gtk::Orientation::Vertical, 12);
    left.set_hexpand(true);

    let layout_panel = gtk::Box::new(gtk::Orientation::Vertical, 10);
    layout_panel.add_css_class("card");

    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let layout_title = gtk::Label::new(Some(tr(language, "layout")));
    layout_title.add_css_class("section-title");
    layout_title.set_xalign(0.0);
    layout_title.set_hexpand(true);
    toolbar.append(&layout_title);

    let view_chip = action_button(
        "view-grid-symbolic",
        if language == Language::Ru {
            "Колонки"
        } else {
            "Column view"
        },
        false,
    );
    view_chip.set_sensitive(false);
    toolbar.append(&view_chip);

    let output_entry = gtk::Entry::new();
    output_entry.set_width_chars(13);
    output_entry.set_text(draft.borrow().output.as_deref().unwrap_or(""));
    output_entry.set_placeholder_text(Some(if language == Language::Ru {
        "Монитор: Авто"
    } else {
        "Output: Auto"
    }));
    output_entry.set_tooltip_text(Some(tr(language, "output")));
    {
        let draft = Rc::clone(&draft);
        output_entry.connect_changed(move |entry| {
            let value = entry.text().trim().to_owned();
            draft.borrow_mut().output = (!value.is_empty()).then_some(value);
        });
    }
    toolbar.append(&output_entry);

    let workspace_entry = gtk::Entry::new();
    workspace_entry.set_width_chars(16);
    workspace_entry.set_text(&draft.borrow().workspace);
    workspace_entry.set_tooltip_text(Some(tr(language, "workspace_name")));
    {
        let draft = Rc::clone(&draft);
        workspace_entry.connect_changed(move |entry| {
            draft.borrow_mut().workspace = entry.text().to_string();
        });
    }
    toolbar.append(&workspace_entry);
    layout_panel.append(&toolbar);

    let weak = Rc::downgrade(state);
    let key_for_select = key.to_owned();
    let draft_for_select = Rc::clone(&draft);
    let callback: Rc<dyn Fn(String)> = Rc::new(move |name| {
        if let Some(state) = weak.upgrade() {
            show_editor_session(
                &state,
                &key_for_select,
                Rc::clone(&draft_for_select),
                Some(name),
            );
        }
    });
    let weak = Rc::downgrade(state);
    let key_for_drop = key.to_owned();
    let draft_for_drop = Rc::clone(&draft);
    let drop_callback: DropCallback = Rc::new(move |dragged, target, after| {
        let Some(state) = weak.upgrade() else {
            return;
        };

        {
            let mut recipe = draft_for_drop.borrow_mut();
            let Some(source_index) = recipe
                .windows
                .iter()
                .position(|window| window.name == dragged)
            else {
                return;
            };

            let mut moved = recipe.windows.remove(source_index);
            let Some(target_index) = recipe
                .windows
                .iter()
                .position(|window| window.name == target)
            else {
                return;
            };

            let target_layout = recipe.windows[target_index].layout.clone();
            moved.layout.column = target_layout.column;
            moved.layout.column_width = None;
            moved.layout.display = target_layout.display;
            moved.layout.floating = false;

            let insert_index = if after {
                target_index + 1
            } else {
                target_index
            };
            recipe.windows.insert(insert_index, moved);
            state::normalize_columns(&mut recipe);
        }

        show_editor_session(
            &state,
            &key_for_drop,
            Rc::clone(&draft_for_drop),
            Some(dragged),
        );
    });

    layout_panel.append(&build_layout_canvas(
        &draft.borrow(),
        selected.as_deref(),
        Some(callback),
        Some(drop_callback),
    ));
    left.append(&layout_panel);

    let details = gtk::Box::new(gtk::Orientation::Vertical, 11);
    details.add_css_class("card");
    details.set_hexpand(true);

    let selected_name = selected.clone().or_else(|| {
        draft
            .borrow()
            .windows
            .first()
            .map(|window| window.name.clone())
    });

    if let Some(selected_name) = selected_name.clone() {
        build_selected_window_controls(
            state,
            &details,
            Rc::clone(&draft),
            &selected_name,
            language,
        );
    } else {
        let empty = gtk::Label::new(Some(if language == Language::Ru {
            "В окружении нет окон."
        } else {
            "This workbench has no windows."
        }));
        empty.add_css_class("subtitle");
        details.append(&empty);
    }
    left.append(&details);
    main.append(&left);

    let windows = gtk::Box::new(gtk::Orientation::Vertical, 9);
    windows.add_css_class("card");
    windows.set_size_request(300, -1);

    let windows_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let windows_title = gtk::Label::new(Some(&format!(
        "{} ({})",
        tr(language, "windows"),
        draft.borrow().windows.len()
    )));
    windows_title.add_css_class("section-title");
    windows_title.set_xalign(0.0);
    windows_title.set_hexpand(true);
    windows_header.append(&windows_title);

    let add_window = action_button(
        "list-add-symbolic",
        if language == Language::Ru {
            "Добавить"
        } else {
            "Add window"
        },
        false,
    );
    let weak = Rc::downgrade(state);
    add_window.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            show_capture(&state);
        }
    });
    windows_header.append(&add_window);
    windows.append(&windows_header);

    for spec in &draft.borrow().windows {
        let button = gtk::Button::new();
        if selected_name.as_deref() == Some(spec.name.as_str()) {
            button.add_css_class("card-selected");
        } else {
            button.add_css_class("secondary-action");
        }

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 9);
        let drag = gtk::Image::from_icon_name("list-drag-handle-symbolic");
        drag.set_pixel_size(15);
        row.append(&drag);
        row.append(&app_icon_tile(spec, 25, true));

        let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
        labels.set_hexpand(true);
        let name = gtk::Label::new(Some(&friendly_window_name(spec)));
        name.set_xalign(0.0);
        labels.append(&name);
        let logical = gtk::Label::new(Some(&spec.name));
        logical.set_xalign(0.0);
        logical.add_css_class("subtitle");
        labels.append(&logical);
        row.append(&labels);

        let more = gtk::Image::from_icon_name("view-more-symbolic");
        more.set_pixel_size(15);
        row.append(&more);
        button.set_child(Some(&row));

        let source = gtk::DragSource::new();
        source.set_actions(gtk::gdk::DragAction::MOVE);
        let drag_name = spec.name.clone();
        source.connect_prepare(move |_, _, _| {
            Some(gtk::gdk::ContentProvider::for_value(&drag_name.to_value()))
        });
        button.add_controller(source);

        let weak = Rc::downgrade(state);
        let key_owned = key.to_owned();
        let draft_owned = Rc::clone(&draft);
        let name_owned = spec.name.clone();
        button.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                show_editor_session(
                    &state,
                    &key_owned,
                    Rc::clone(&draft_owned),
                    Some(name_owned.clone()),
                );
            }
        });
        windows.append(&button);
    }

    let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    windows.append(&spacer);

    let help_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let info = gtk::Image::from_icon_name("dialog-information-symbolic");
    info.set_pixel_size(17);
    info.set_valign(gtk::Align::Start);
    help_row.append(&info);
    let help = gtk::Label::new(Some(tr(language, "open_hint")));
    help.set_xalign(0.0);
    help.set_wrap(true);
    help.set_max_width_chars(30);
    help.add_css_class("subtitle");
    help_row.append(&help);
    windows.append(&help_row);

    main.append(&windows);
    page.append(&main);

    let weak = Rc::downgrade(state);
    let key_for_save = key.to_owned();
    let draft_for_save = Rc::clone(&draft);
    save.connect_clicked(move |_| {
        let Some(state) = weak.upgrade() else {
            return;
        };

        let mut recipe = draft_for_save.borrow().clone();
        state::normalize_columns(&mut recipe);

        let mut next = state.config.borrow().clone();
        next.workbench.insert(key_for_save.clone(), recipe.clone());

        match state::persist_config(&state.config_path, &next) {
            Ok(()) => {
                *state.config.borrow_mut() = next;
                *draft_for_save.borrow_mut() = recipe;
                toast_message(&state, tr(state.language.get(), "saved"));
                show_editor_session(
                    &state,
                    &key_for_save,
                    Rc::clone(&draft_for_save),
                    selected_name.clone(),
                );
            }
            Err(error) => toast_message(&state, &error.to_string()),
        }
    });

    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&page)
        .build()
}

fn show_layout_preview(state: &Rc<UiState>, recipe: &Recipe) {
    let language = state.language.get();
    let preview = gtk::Window::builder()
        .title(if language == Language::Ru {
            "Предпросмотр макета"
        } else {
            "Layout preview"
        })
        .transient_for(&state.window)
        .modal(true)
        .default_width(820)
        .default_height(520)
        .build();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 14);
    root.add_css_class("content");

    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let title = gtk::Label::new(Some(if language == Language::Ru {
        "Предпросмотр макета"
    } else {
        "Layout preview"
    }));
    title.add_css_class("title-xl");
    title.set_xalign(0.0);
    title.set_hexpand(true);
    heading.append(&title);

    let close = action_button(
        "window-close-symbolic",
        if language == Language::Ru {
            "Закрыть"
        } else {
            "Close"
        },
        false,
    );
    let preview_copy = preview.clone();
    close.connect_clicked(move |_| preview_copy.close());
    heading.append(&close);
    root.append(&heading);

    let subtitle = gtk::Label::new(Some(&format!(
        "{} · {}",
        recipe.workspace,
        if language == Language::Ru {
            "так будет выглядеть сохранённая структура"
        } else {
            "saved layout structure"
        }
    )));
    subtitle.set_xalign(0.0);
    subtitle.add_css_class("subtitle");
    root.append(&subtitle);

    let canvas = build_layout_canvas(recipe, None, None, None);
    canvas.set_vexpand(true);
    root.append(&canvas);
    preview.set_child(Some(&root));
    preview.present();
}

fn build_selected_window_controls(
    _state: &Rc<UiState>,
    container: &gtk::Box,
    draft: Rc<RefCell<Recipe>>,
    selected_name: &str,
    language: Language,
) {
    let Some(index) = draft
        .borrow()
        .windows
        .iter()
        .position(|window| window.name == selected_name)
    else {
        return;
    };
    let current = draft.borrow().windows[index].clone();

    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 11);
    heading.append(&app_icon_tile(&current, 34, false));

    let heading_text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    heading_text.set_hexpand(true);
    let title = gtk::Label::new(Some(&friendly_window_name(&current)));
    title.add_css_class("title-lg");
    title.set_xalign(0.0);
    heading_text.append(&title);

    let logical = gtk::Label::new(Some(&format!(
        "{} · {}",
        current.name,
        current
            .match_spec
            .app_id
            .as_deref()
            .unwrap_or("application")
            .trim_matches('^')
            .trim_matches('$')
    )));
    logical.set_xalign(0.0);
    logical.add_css_class("subtitle");
    heading_text.append(&logical);
    heading.append(&heading_text);
    container.append(&heading);

    let command = gtk::Entry::new();
    command.set_text(&state::display_command(&current.command));
    command.set_placeholder_text(Some(tr(language, "reuse_only")));
    container.append(&labeled_control(tr(language, "launch_command"), &command));
    {
        let draft = Rc::clone(&draft);
        command.connect_changed(move |entry| {
            if let Ok(command) = state::parse_command(entry.text().as_str()) {
                if let Some(window) = draft.borrow_mut().windows.get_mut(index) {
                    window.command = command;
                }
            }
        });
    }

    let reuse = gtk::DropDown::from_strings(&[
        if language == Language::Ru {
            "Переиспользовать подходящее окно"
        } else {
            "Reuse existing matching window"
        },
        if language == Language::Ru {
            "Всегда запускать новое"
        } else {
            "Always launch a new window"
        },
    ]);
    reuse.set_selected(match current.reuse {
        ReusePolicy::Unique => 0,
        ReusePolicy::Never => 1,
    });
    container.append(&labeled_control(
        if language == Language::Ru {
            "Правило запуска"
        } else {
            "Reuse rule"
        },
        &reuse,
    ));
    {
        let draft = Rc::clone(&draft);
        reuse.connect_selected_notify(move |dropdown| {
            if let Some(window) = draft.borrow_mut().windows.get_mut(index) {
                window.reuse = if dropdown.selected() == 0 {
                    ReusePolicy::Unique
                } else {
                    ReusePolicy::Never
                };
            }
        });
    }

    match current.layout.column_width {
        Some(Size::Percent(value)) => {
            let width_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 10.0, 100.0, 1.0);
            scale.set_value((value * 100.0).clamp(10.0, 100.0));
            scale.set_draw_value(false);
            scale.set_hexpand(true);
            width_box.append(&scale);

            let value_label = gtk::Label::new(Some(&format!("{:.0}%", value * 100.0)));
            value_label.set_width_chars(5);
            width_box.append(&value_label);

            container.append(&labeled_control(tr(language, "column_width"), &width_box));

            let draft = Rc::clone(&draft);
            scale.connect_value_changed(move |scale| {
                let percent = scale.value().clamp(10.0, 100.0);
                value_label.set_text(&format!("{percent:.0}%"));
                if let Some(window) = draft.borrow_mut().windows.get_mut(index) {
                    window.layout.column_width = Some(Size::Percent(percent / 100.0));
                }
            });
        }
        other => {
            let width = gtk::Entry::new();
            width.set_placeholder_text(Some(tr(language, "auto")));
            if let Some(size) = other {
                width.set_text(&size.to_string());
            }
            container.append(&labeled_control(tr(language, "column_width"), &width));
            let draft = Rc::clone(&draft);
            width.connect_changed(move |entry| {
                let text = entry.text();
                let value = if text.trim().is_empty() {
                    Some(None)
                } else {
                    Size::from_str(text.trim()).ok().map(Some)
                };
                if let Some(value) = value {
                    if let Some(window) = draft.borrow_mut().windows.get_mut(index) {
                        window.layout.column_width = value;
                    }
                }
            });
        }
    }

    let focus_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let focus = gtk::Switch::new();
    focus.set_active(draft.borrow().focus.as_deref() == Some(current.name.as_str()));
    focus_row.append(&focus);
    let focus_help = gtk::Label::new(Some(if language == Language::Ru {
        "Фокусировать это окно после открытия окружения."
    } else {
        "Focus this window after opening the workbench."
    }));
    focus_help.set_xalign(0.0);
    focus_help.set_wrap(true);
    focus_help.add_css_class("subtitle");
    focus_row.append(&focus_help);
    container.append(&labeled_control(tr(language, "final_focus"), &focus_row));
    {
        let draft = Rc::clone(&draft);
        let name = current.name.clone();
        focus.connect_active_notify(move |switch| {
            let mut recipe = draft.borrow_mut();
            if switch.is_active() {
                recipe.focus = Some(name.clone());
            } else if recipe.focus.as_deref() == Some(name.as_str()) {
                recipe.focus = None;
            }
        });
    }

    let advanced = gtk::Expander::new(Some(tr(language, "advanced")));
    let advanced_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    advanced_box.set_margin_top(10);

    let column = gtk::SpinButton::with_range(1.0, 24.0, 1.0);
    column.set_value(current.layout.column as f64);
    advanced_box.append(&labeled_control(tr(language, "column"), &column));
    {
        let draft = Rc::clone(&draft);
        column.connect_value_changed(move |spin| {
            if let Some(window) = draft.borrow_mut().windows.get_mut(index) {
                window.layout.column = spin.value_as_int().max(1) as usize;
            }
        });
    }

    let height = gtk::Entry::new();
    height.set_placeholder_text(Some(tr(language, "auto")));
    if let Some(size) = current.layout.window_height {
        height.set_text(&size.to_string());
    }
    advanced_box.append(&labeled_control(tr(language, "window_height"), &height));
    {
        let draft = Rc::clone(&draft);
        height.connect_changed(move |entry| {
            let text = entry.text();
            let value = if text.trim().is_empty() {
                Some(None)
            } else {
                Size::from_str(text.trim()).ok().map(Some)
            };
            if let Some(value) = value {
                if let Some(window) = draft.borrow_mut().windows.get_mut(index) {
                    window.layout.window_height = value;
                }
            }
        });
    }

    let display = gtk::DropDown::from_strings(&[
        tr(language, "not_set"),
        tr(language, "normal"),
        tr(language, "tabbed"),
    ]);
    display.set_selected(match current.layout.display {
        None => 0,
        Some(ColumnDisplay::Normal) => 1,
        Some(ColumnDisplay::Tabbed) => 2,
    });
    advanced_box.append(&labeled_control(tr(language, "display_mode"), &display));
    {
        let draft = Rc::clone(&draft);
        display.connect_selected_notify(move |dropdown| {
            let display = match dropdown.selected() {
                1 => Some(ColumnDisplay::Normal),
                2 => Some(ColumnDisplay::Tabbed),
                _ => None,
            };
            let column = draft
                .borrow()
                .windows
                .get(index)
                .map(|window| window.layout.column)
                .unwrap_or(1);
            state::set_column_display(&mut draft.borrow_mut(), column, display);
        });
    }

    let app_id = gtk::Entry::new();
    app_id.set_text(current.match_spec.app_id.as_deref().unwrap_or(""));
    advanced_box.append(&labeled_control(tr(language, "match_app"), &app_id));
    {
        let draft = Rc::clone(&draft);
        app_id.connect_changed(move |entry| {
            let value = entry.text().trim().to_owned();
            if let Some(window) = draft.borrow_mut().windows.get_mut(index) {
                window.match_spec.app_id = (!value.is_empty()).then_some(value);
            }
        });
    }

    let title_match = gtk::Entry::new();
    title_match.set_text(current.match_spec.title.as_deref().unwrap_or(""));
    advanced_box.append(&labeled_control(tr(language, "match_title"), &title_match));
    {
        let draft = Rc::clone(&draft);
        title_match.connect_changed(move |entry| {
            let value = entry.text().trim().to_owned();
            if let Some(window) = draft.borrow_mut().windows.get_mut(index) {
                window.match_spec.title = (!value.is_empty()).then_some(value);
            }
        });
    }

    advanced.set_child(Some(&advanced_box));
    container.append(&advanced);
}

fn labeled_control(label_text: &str, control: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let label = gtk::Label::new(Some(label_text));
    label.set_width_chars(18);
    label.set_xalign(0.0);
    row.append(&label);
    control.set_hexpand(true);
    row.append(control);
    row
}

fn error_page(
    state: &Rc<UiState>,
    title_text: &str,
    body_text: &str,
    retry: Option<&str>,
) -> gtk::ScrolledWindow {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    page.add_css_class("content");

    let card = gtk::Box::new(gtk::Orientation::Vertical, 10);
    card.add_css_class("card");

    let title = gtk::Label::new(Some(title_text));
    title.add_css_class("title-lg");
    title.set_xalign(0.0);
    card.append(&title);

    let body = gtk::Label::new(Some(body_text));
    body.set_xalign(0.0);
    body.set_wrap(true);
    body.add_css_class("subtitle");
    card.append(&body);

    if retry == Some("capture") {
        let button = gtk::Button::with_label(tr(state.language.get(), "refresh"));
        button.add_css_class("secondary-action");
        button.set_halign(gtk::Align::Start);
        let weak = Rc::downgrade(state);
        button.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                show_capture(&state);
            }
        });
        card.append(&button);
    }

    page.append(&card);
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&page)
        .build()
}

pub fn build_quick_launcher(app: &adw::Application) {
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);

    let language = settings::load_language(&settings::settings_path());
    let config = state::load_config_or_empty(&settings::config_path()).unwrap_or(Config {
        workbench: Default::default(),
    });
    let snapshot = state::niri_snapshot().ok();

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(tr(language, "quick_launcher"))
        .default_width(760)
        .default_height(570)
        .build();
    window.add_css_class("quick-window");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 13);
    root.add_css_class("quick-root");

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 9);
    header.append(&traffic_dot("traffic-red"));
    header.append(&traffic_dot("traffic-yellow"));
    header.append(&traffic_dot("traffic-green"));

    let title = gtk::Label::new(Some(tr(language, "quick_launcher")));
    title.add_css_class("title-lg");
    title.set_xalign(0.0);
    title.set_hexpand(true);
    title.set_margin_start(10);
    header.append(&title);

    let esc = gtk::Label::new(Some("Esc"));
    esc.add_css_class("secondary-action");
    header.append(&esc);
    let close = gtk::Button::with_label(if language == Language::Ru {
        "Закрыть"
    } else {
        "Close"
    });
    close.add_css_class("flat");
    let quick_window = window.clone();
    close.connect_clicked(move |_| quick_window.close());
    header.append(&close);
    root.append(&header);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(tr(language, "search")));
    search.add_css_class("search");
    root.append(&search);

    let list = gtk::Box::new(gtk::Orientation::Vertical, 9);
    let items: Rc<RefCell<Vec<(String, String, gtk::Button)>>> = Rc::new(RefCell::new(Vec::new()));

    let mut first_summary = None::<String>;
    for (index, (key, recipe)) in config.workbench.iter().enumerate() {
        let status = state::recipe_status(recipe, snapshot.as_ref());
        let row = gtk::Button::new();
        row.add_css_class(if index == 0 {
            "card-selected"
        } else {
            "quick-result"
        });

        let inside = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let icons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        for spec in recipe.windows.iter().take(3) {
            icons.append(&app_icon_tile(spec, 28, true));
        }
        inside.append(&icons);

        let labels = gtk::Box::new(gtk::Orientation::Vertical, 4);
        labels.set_hexpand(true);
        let name = gtk::Label::new(Some(recipe_display_name(recipe)));
        name.set_xalign(0.0);
        name.add_css_class("title-lg");
        labels.append(&name);

        let detail_text = recipe
            .windows
            .iter()
            .map(friendly_window_name)
            .collect::<Vec<_>>()
            .join(" · ");
        let detail = gtk::Label::new(Some(&detail_text));
        detail.set_xalign(0.0);
        detail.add_css_class("subtitle");
        labels.append(&detail);
        labels.append(&status_badge(language, &status));
        inside.append(&labels);

        let action_box = gtk::Box::new(gtk::Orientation::Vertical, 5);
        action_box.set_valign(gtk::Align::Center);
        let primary_action = if matches!(status, RecipeStatus::LayoutChanged) {
            tr(language, "repair")
        } else {
            tr(language, "open")
        };
        let action = gtk::Label::new(Some(&format!("Enter · {primary_action}")));
        action.add_css_class(if index == 0 {
            "primary-action"
        } else {
            "secondary-action"
        });
        action.add_css_class("action-button");
        action_box.append(&action);
        let repair_hint =
            gtk::Label::new(Some(&format!("Ctrl+Enter · {}", tr(language, "repair"))));
        repair_hint.add_css_class("subtitle");
        action_box.append(&repair_hint);
        inside.append(&action_box);

        row.set_child(Some(&inside));

        let key_owned = key.clone();
        let launch_action = if matches!(status, RecipeStatus::LayoutChanged) {
            "repair"
        } else {
            "open"
        };
        let quick_window = window.clone();
        row.connect_clicked(move |_| {
            if state::launch_cli(launch_action, &key_owned).is_ok() {
                quick_window.close();
            }
        });

        if index == 0 {
            let total = recipe.windows.len();
            let (reuse, launch) = match status {
                RecipeStatus::Missing(missing) => (total.saturating_sub(missing), missing),
                RecipeStatus::Ready | RecipeStatus::LayoutChanged => (total, 0),
                RecipeStatus::Ambiguous | RecipeStatus::Offline => (0, 0),
            };
            first_summary = Some(if language == Language::Ru {
                format!(
                    "Переиспользует {reuse}, запустит {launch}, восстановит workspace {}.",
                    recipe.workspace
                )
            } else {
                format!(
                    "Will reuse {reuse} app{}, launch {launch}, restore layout on workspace {}.",
                    if reuse == 1 { "" } else { "s" },
                    recipe.workspace
                )
            });
        }

        items.borrow_mut().push((
            format!("{key} {} {}", recipe_display_name(recipe), recipe.workspace)
                .to_ascii_lowercase(),
            key.clone(),
            row.clone(),
        ));
        list.append(&row);
    }

    let items_for_search = Rc::clone(&items);
    search.connect_search_changed(move |entry| {
        let query = entry.text().to_ascii_lowercase();
        for (haystack, _, button) in items_for_search.borrow().iter() {
            button.set_visible(query.is_empty() || haystack.contains(&query));
        }
    });

    let items_for_enter = Rc::clone(&items);
    search.connect_activate(move |_| {
        if let Some((_, _, button)) = items_for_enter
            .borrow()
            .iter()
            .find(|(_, _, button)| button.is_visible())
        {
            button.emit_clicked();
        }
    });
    root.append(&list);

    if let Some(summary) = first_summary {
        let summary_row = gtk::Box::new(gtk::Orientation::Horizontal, 9);
        summary_row.add_css_class("footer");
        let info = gtk::Image::from_icon_name("dialog-information-symbolic");
        info.set_pixel_size(17);
        summary_row.append(&info);
        let label = gtk::Label::new(Some(&summary));
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.add_css_class("subtitle");
        summary_row.append(&label);
        root.append(&summary_row);
    }

    let hint_row = gtk::Box::new(gtk::Orientation::Horizontal, 9);
    hint_row.add_css_class("footer");
    let keyboard = gtk::Image::from_icon_name("input-keyboard-symbolic");
    keyboard.set_pixel_size(17);
    hint_row.append(&keyboard);
    let hint = gtk::Label::new(Some(tr(language, "quick_hint")));
    hint.set_xalign(0.0);
    hint.add_css_class("subtitle");
    hint_row.append(&hint);
    root.append(&hint_row);

    let controller = gtk::EventControllerKey::new();
    let items_for_repair = Rc::clone(&items);
    let quick_window = window.clone();
    controller.connect_key_pressed(move |_, key, _, modifiers| {
        if key == gtk::gdk::Key::Escape {
            quick_window.close();
            return glib::Propagation::Stop;
        }
        if key == gtk::gdk::Key::Return && modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
        {
            if let Some((_, workbench, _)) = items_for_repair
                .borrow()
                .iter()
                .find(|(_, _, button)| button.is_visible())
            {
                if state::launch_cli("repair", workbench).is_ok() {
                    quick_window.close();
                }
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    window.add_controller(controller);

    window.set_content(Some(&root));
    window.present();
    state::shape_own_window(780, 570);
    search.grab_focus();
}
