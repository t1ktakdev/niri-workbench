use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gtk4 as gtk;
use libadwaita as adw;
use regex::Regex;

use adw::prelude::*;
use gtk::glib;
use gtk::glib::value::ToValue;
use workbench_core::{
    ColumnDisplay, Config, MatchSpec, PlacementSpec, Recipe, ReusePolicy, Size, WindowSpec,
};

use crate::i18n::{Language, tr};
use crate::settings;
use crate::state::{self, RecipeStatus};

type DropCallback = Rc<dyn Fn(String, String, bool)>;
type SimpleDropCallback = Rc<dyn Fn(String)>;
type ValidationErrors = Rc<RefCell<HashMap<String, String>>>;
type StateAction = Rc<dyn Fn(&Rc<UiState>)>;

#[derive(Clone)]
struct EditorGuard {
    key: String,
    draft: Rc<RefCell<Recipe>>,
    validation_errors: ValidationErrors,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HomeFilter {
    All,
    Ready,
    NeedsAttention,
}

impl HomeFilter {
    fn matches(self, status: &RecipeStatus) -> bool {
        match self {
            Self::All => true,
            Self::Ready => matches!(status, RecipeStatus::Ready),
            Self::NeedsAttention => !matches!(status, RecipeStatus::Ready),
        }
    }
}

#[derive(Clone)]
struct QuickLauncherItem {
    haystack: String,
    key: String,
    status: RecipeStatus,
    button: gtk::Button,
    summary: String,
}

fn quick_launch_action(status: &RecipeStatus) -> Option<&'static str> {
    match status {
        RecipeStatus::Ready | RecipeStatus::Missing(_) => Some("open"),
        RecipeStatus::LayoutChanged => Some("repair"),
        RecipeStatus::Ambiguous | RecipeStatus::Offline => None,
    }
}

fn quick_summary(language: Language, recipe: &Recipe, status: &RecipeStatus) -> String {
    match status {
        RecipeStatus::Offline => {
            if language == Language::Ru {
                "Niri IPC недоступен. Окружение нельзя открыть из launcher прямо сейчас.".to_owned()
            } else {
                "Niri IPC is unavailable. This workbench cannot be opened right now.".to_owned()
            }
        }
        RecipeStatus::Ambiguous => {
            if language == Language::Ru {
                "Найдено несколько подходящих окон. Открой Workbench и уточни matcher.".to_owned()
            } else {
                "Multiple windows match this recipe. Open Workbench and refine its matcher."
                    .to_owned()
            }
        }
        _ => {
            let total = recipe.windows.len();
            let (reuse, launch) = match status {
                RecipeStatus::Missing(missing) => (total.saturating_sub(*missing), *missing),
                RecipeStatus::Ready | RecipeStatus::LayoutChanged => (total, 0),
                RecipeStatus::Ambiguous | RecipeStatus::Offline => unreachable!(),
            };
            if language == Language::Ru {
                format!(
                    "Переиспользует {reuse}, запустит {launch}, workspace: {}.",
                    recipe.workspace
                )
            } else {
                format!(
                    "Will reuse {reuse} app{}, launch {launch}, workspace: {}.",
                    if reuse == 1 { "" } else { "s" },
                    recipe.workspace
                )
            }
        }
    }
}

fn set_quick_selection(
    items: &Rc<RefCell<Vec<QuickLauncherItem>>>,
    selected: &Rc<Cell<Option<usize>>>,
    summary: &gtk::Label,
    requested: Option<usize>,
) {
    let items_ref = items.borrow();
    let next = requested
        .filter(|index| {
            items_ref
                .get(*index)
                .is_some_and(|item| item.button.is_visible())
        })
        .or_else(|| items_ref.iter().position(|item| item.button.is_visible()));

    selected.set(next);
    for (index, item) in items_ref.iter().enumerate() {
        if Some(index) == next {
            item.button.add_css_class("quick-selected");
        } else {
            item.button.remove_css_class("quick-selected");
        }
    }

    if let Some(index) = next {
        summary.set_text(&items_ref[index].summary);
        summary.set_visible(true);
    } else {
        summary.set_visible(false);
    }
}

fn move_quick_selection(
    items: &Rc<RefCell<Vec<QuickLauncherItem>>>,
    selected: &Rc<Cell<Option<usize>>>,
    summary: &gtk::Label,
    delta: isize,
) {
    let visible = items
        .borrow()
        .iter()
        .enumerate()
        .filter_map(|(index, item)| item.button.is_visible().then_some(index))
        .collect::<Vec<_>>();
    if visible.is_empty() {
        set_quick_selection(items, selected, summary, None);
        return;
    }

    let current_position = selected
        .get()
        .and_then(|current| visible.iter().position(|index| *index == current))
        .unwrap_or(0) as isize;
    let next_position =
        (current_position + delta).rem_euclid(isize::try_from(visible.len()).unwrap_or(1));
    set_quick_selection(
        items,
        selected,
        summary,
        visible.get(next_position as usize).copied(),
    );
}

thread_local! {
    static LIVE_STATES: RefCell<Vec<Rc<UiState>>> = const { RefCell::new(Vec::new()) };
}

fn forget_live_state(state: &Rc<UiState>) {
    LIVE_STATES.with(|states| {
        states
            .borrow_mut()
            .retain(|candidate| !Rc::ptr_eq(candidate, state));
    });
}

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
    editor_guard: RefCell<Option<EditorGuard>>,
    allow_close: Cell<bool>,
}

#[derive(Debug, Clone)]
pub enum StartPage {
    Capture,
    Library,
    Settings,
    Edit(String),
}

fn route_start_page(state: &Rc<UiState>, start_page: StartPage) {
    let current = visible_page_name(state);
    let same_protected_page = match (&start_page, current.as_deref()) {
        (StartPage::Capture, Some("capture")) => true,
        (StartPage::Edit(key), Some("editor")) => state
            .editor_guard
            .borrow()
            .as_ref()
            .is_some_and(|guard| guard.key == *key),
        _ => false,
    };

    if same_protected_page {
        return;
    }

    if matches!(current.as_deref(), Some("editor" | "capture")) {
        toast_message(
            state,
            if state.language.get() == Language::Ru {
                "Сначала закончи редактирование или захват."
            } else {
                "Finish editing or capture before opening another page."
            },
        );
        return;
    }

    match start_page {
        StartPage::Capture => show_capture(state),
        StartPage::Library => show_library(state),
        StartPage::Settings => show_settings(state),
        StartPage::Edit(key) => show_editor(state, &key),
    }
}

pub fn present_main_window(app: &adw::Application, start_page: Option<StartPage>) {
    let existing = LIVE_STATES.with(|states| states.borrow().last().cloned());
    if let Some(state) = existing {
        let was_visible = state.window.is_visible();
        if let Some(start_page) = start_page {
            route_start_page(&state, start_page);
        }
        state.window.present();
        if !was_visible {
            state::shape_own_window(1240, 780);
        }
        return;
    }

    build_main_window(app, start_page);
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
        .default_width(1240)
        .default_height(780)
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
        editor_guard: RefCell::new(None),
        allow_close: Cell::new(false),
    });

    LIVE_STATES.with(|states| states.borrow_mut().push(Rc::clone(&state)));
    let weak = Rc::downgrade(&state);
    state.window.connect_close_request(move |_| {
        let Some(state) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };

        if state.allow_close.get() {
            state.allow_close.set(false);
            forget_live_state(&state);
            return glib::Propagation::Proceed;
        }

        if let Some(guard) = state.editor_guard.borrow().clone() {
            if editor_requires_confirmation(
                &state,
                &guard.key,
                &guard.draft,
                &guard.validation_errors,
            ) {
                let (message, detail) = if state.language.get() == Language::Ru {
                    (
                        "Закрыть без сохранения?",
                        "Изменения в редакторе ещё не сохранены.",
                    )
                } else {
                    (
                        "Close without saving?",
                        "Editor changes have not been saved yet.",
                    )
                };
                let on_discard: StateAction = Rc::new(|state| {
                    state.allow_close.set(true);
                    state.window.close();
                });
                show_discard_confirmation(&state, message, detail, on_discard);
                return glib::Propagation::Stop;
            }
        }

        if visible_page_name(&state).as_deref() == Some("capture") {
            let (message, detail) = if state.language.get() == Language::Ru {
                (
                    "Закрыть незавершённый захват?",
                    "Черновик Capture ещё не сохранён как окружение.",
                )
            } else {
                (
                    "Close unfinished capture?",
                    "The Capture draft has not been saved as a workbench yet.",
                )
            };
            let on_discard: StateAction = Rc::new(|state| {
                state.allow_close.set(true);
                state.window.close();
            });
            show_discard_confirmation(&state, message, detail, on_discard);
            return glib::Propagation::Stop;
        }

        forget_live_state(&state);
        glib::Propagation::Proceed
    });

    rebuild_shell(&state);
    if let Some(start_page) = start_page {
        route_start_page(&state, start_page);
    }
    state.window.present();
    state::shape_own_window(1240, 780);

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
    button.add_css_class(if primary { "suggested-action" } else { "flat" });

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
    button.add_css_class("flat");
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

    let icon_name = state::icon_name(spec);
    let image = gtk::Image::from_icon_name(&icon_name);
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

fn visible_page_name(state: &Rc<UiState>) -> Option<String> {
    state
        .stack
        .borrow()
        .as_ref()
        .and_then(|stack| stack.visible_child_name())
        .map(|name| name.to_string())
}

fn build_language_switch(state: &Rc<UiState>) -> gtk::Box {
    let language = state.language.get();
    let language_switch = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    language_switch.add_css_class("language-switch");

    for (label, next) in [("RU", Language::Ru), ("EN", Language::En)] {
        let button = gtk::Button::with_label(label);
        button.add_css_class("language-button");
        if language == next {
            button.add_css_class("language-active");
        }

        let weak = Rc::downgrade(state);
        button.connect_clicked(move |_| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            if next == state.language.get() {
                return;
            }

            let page = visible_page_name(&state).unwrap_or_else(|| "home".to_owned());
            if matches!(page.as_str(), "editor" | "capture") {
                toast_message(
                    &state,
                    if state.language.get() == Language::Ru {
                        "Сначала закончи редактирование или захват."
                    } else {
                        "Finish editing or capture before changing language."
                    },
                );
                return;
            }

            state.language.set(next);
            if let Err(error) = settings::save_language(&state.settings_path, next) {
                toast_message(&state, &error.to_string());
            }
            rebuild_shell(&state);
            match page.as_str() {
                "library" => show_library(&state),
                "settings" => show_settings(&state),
                _ => show_home(&state),
            }
        });
        language_switch.append(&button);
    }

    language_switch
}

fn rebuild_shell(state: &Rc<UiState>) {
    clear_box(&state.shell);

    let language = state.language.get();
    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(true);
    header.set_show_end_title_buttons(false);
    header.set_decoration_layout(Some("close,maximize:"));

    let title = gtk::Label::new(Some("Workbench"));
    title.add_css_class("heading");
    header.set_title_widget(Some(&title));

    let hide_label = if language == Language::Ru {
        "Скрыть"
    } else {
        "Hide"
    };
    let hide = gtk::Button::from_icon_name("window-minimize-symbolic");
    hide.add_css_class("flat");
    hide.set_tooltip_text(Some(hide_label));
    hide.update_property(&[gtk::accessible::Property::Label(hide_label)]);
    let weak = Rc::downgrade(state);
    hide.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            state.window.set_visible(false);
        }
    });
    header.pack_start(&hide);

    header.pack_end(&build_language_switch(state));

    let new_button = action_button("list-add-symbolic", tr(language, "new"), true);
    new_button.set_tooltip_text(Some(tr(language, "new")));
    let weak = Rc::downgrade(state);
    new_button.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            match visible_page_name(&state).as_deref() {
                Some("editor") => toast_message(
                    &state,
                    if state.language.get() == Language::Ru {
                        "Сначала сохрани или отмени изменения в редакторе."
                    } else {
                        "Save or cancel the editor changes first."
                    },
                ),
                Some("capture") => toast_message(
                    &state,
                    if state.language.get() == Language::Ru {
                        "Захват уже открыт."
                    } else {
                        "Capture is already open."
                    },
                ),
                _ => show_capture(&state),
            }
        }
    });
    header.pack_end(&new_button);
    state.shell.append(&header);

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.set_hexpand(true);
    body.set_vexpand(true);

    let sidebar = build_sidebar(state);
    body.append(&sidebar);

    let stack = gtk::Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    stack.set_hhomogeneous(false);
    stack.set_vhomogeneous(false);
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

            let open_page = {
                let page = page.clone();
                Rc::new(move |state: &Rc<UiState>| match page.as_str() {
                    "home" => show_home(state),
                    "capture" => show_capture(state),
                    "library" => show_library(state),
                    "settings" => show_settings(state),
                    _ => {}
                }) as StateAction
            };

            if visible_page_name(&state).as_deref() == Some("capture") && page.as_str() != "capture"
            {
                let (message, detail) = if state.language.get() == Language::Ru {
                    (
                        "Покинуть незавершённый захват?",
                        "Черновик Capture ещё не сохранён как окружение.",
                    )
                } else {
                    (
                        "Leave unfinished capture?",
                        "The Capture draft has not been saved as a workbench yet.",
                    )
                };
                show_discard_confirmation(&state, message, detail, open_page);
                return;
            }

            open_page(&state);
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

    sidebar
}

fn reload_config_from_disk(state: &Rc<UiState>, notify: bool) -> bool {
    match state::load_config_or_empty(&state.config_path) {
        Ok(config) => {
            *state.config.borrow_mut() = config;
            *state.load_error.borrow_mut() = None;
            if notify {
                toast_message(
                    state,
                    if state.language.get() == Language::Ru {
                        "Конфигурация перечитана"
                    } else {
                        "Configuration reloaded"
                    },
                );
            }
            true
        }
        Err(error) => {
            let message = error.to_string();
            *state.load_error.borrow_mut() = Some(message.clone());
            toast_message(
                state,
                &format!("{}: {message}", tr(state.language.get(), "config_error")),
            );
            false
        }
    }
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
    let editing = name == "editor";
    for button in state.nav.borrow().values() {
        button.set_sensitive(!editing);
    }

    if !editing {
        *state.editor_guard.borrow_mut() = None;
        state.window.remove_action("add-window");
        state.window.remove_action("save-editor");
        state.window.remove_action("leave-editor");
        if let Some(app) = state.window.application() {
            app.set_accels_for_action("win.add-window", &[]);
            app.set_accels_for_action("win.save-editor", &[]);
            app.set_accels_for_action("win.leave-editor", &[]);
        }
    }

    let stack_ref = state.stack.borrow();
    let Some(stack) = stack_ref.as_ref() else {
        return;
    };
    let page_changed = stack
        .visible_child_name()
        .is_none_or(|visible| visible.as_str() != name);
    if let Some(old) = stack.child_by_name(name) {
        stack.remove(&old);
    }
    stack.add_named(page, Some(name));
    stack.set_visible_child_name(name);
    drop(stack_ref);

    if page_changed {
        state::ensure_own_window_size(1240, 780);
    }
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
    let page = gtk::Box::new(gtk::Orientation::Vertical, 14);
    page.add_css_class("content");
    page.add_css_class("home-content");

    let search_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(tr(language, "search")));
    search.add_css_class("search");
    search.set_hexpand(true);
    search_row.append(&search);

    let filter_button = gtk::MenuButton::new();
    filter_button.add_css_class("filter-button");
    filter_button.set_tooltip_text(Some(if language == Language::Ru {
        "Фильтр окружений"
    } else {
        "Filter workbenches"
    }));
    let filter_icon = gtk::Image::from_icon_name("view-filter-symbolic");
    filter_icon.set_pixel_size(18);
    filter_button.set_child(Some(&filter_icon));

    let filter_popover = gtk::Popover::new();
    let filter_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    filter_box.add_css_class("filter-popover");

    let filter_title = gtk::Label::new(Some(if language == Language::Ru {
        "Показывать"
    } else {
        "Show"
    }));
    filter_title.set_xalign(0.0);
    filter_title.add_css_class("popover-title");
    filter_box.append(&filter_title);

    let all_filter = gtk::CheckButton::with_label(if language == Language::Ru {
        "Все"
    } else {
        "All"
    });
    all_filter.set_active(true);
    let ready_filter = gtk::CheckButton::with_label(if language == Language::Ru {
        "Готовые"
    } else {
        "Ready"
    });
    ready_filter.set_group(Some(&all_filter));
    let attention_filter = gtk::CheckButton::with_label(if language == Language::Ru {
        "Требуют внимания"
    } else {
        "Needs attention"
    });
    attention_filter.set_group(Some(&all_filter));
    filter_box.append(&all_filter);
    filter_box.append(&ready_filter);
    filter_box.append(&attention_filter);

    let refresh = gtk::Button::with_label(tr(language, "refresh"));
    refresh.add_css_class("popover-action");
    let weak = Rc::downgrade(state);
    refresh.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            reload_config_from_disk(&state, true);
            show_home(&state);
        }
    });
    filter_box.append(&refresh);

    filter_popover.set_child(Some(&filter_box));
    filter_button.set_popover(Some(&filter_popover));
    search_row.append(&filter_button);
    page.append(&search_row);

    let list = gtk::Box::new(gtk::Orientation::Vertical, 10);
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
        let filter_items: Rc<RefCell<Vec<(String, RecipeStatus, gtk::Widget)>>> =
            Rc::new(RefCell::new(Vec::new()));
        let filter_state = Rc::new(Cell::new(HomeFilter::All));

        for (key, recipe) in &config.workbench {
            let status = state::recipe_status(recipe, snapshot.as_ref());
            let card = build_workbench_card(state, key, recipe, status.clone());
            let apps = recipe
                .windows
                .iter()
                .map(friendly_window_name)
                .collect::<Vec<_>>()
                .join(" ");
            filter_items.borrow_mut().push((
                format!(
                    "{key} {} {} {apps}",
                    recipe_display_name(recipe),
                    recipe.workspace
                )
                .to_ascii_lowercase(),
                status,
                card.clone().upcast(),
            ));
            list.append(&card);
        }

        let no_results = gtk::Box::new(gtk::Orientation::Vertical, 7);
        no_results.add_css_class("empty-filter");
        no_results.set_visible(false);
        let no_results_icon = gtk::Image::from_icon_name("system-search-symbolic");
        no_results_icon.set_pixel_size(28);
        no_results.append(&no_results_icon);
        let no_results_title = gtk::Label::new(Some(if language == Language::Ru {
            "Ничего не найдено"
        } else {
            "Nothing found"
        }));
        no_results_title.add_css_class("section-title");
        no_results.append(&no_results_title);
        let no_results_hint = gtk::Label::new(Some(if language == Language::Ru {
            "Измени запрос или сбрось фильтр."
        } else {
            "Try another search or clear the filter."
        }));
        no_results_hint.add_css_class("subtitle");
        no_results.append(&no_results_hint);
        list.append(&no_results);

        let apply_filters: Rc<dyn Fn()> = {
            let items = Rc::clone(&filter_items);
            let filter_state = Rc::clone(&filter_state);
            let search = search.clone();
            let no_results = no_results.clone();
            Rc::new(move || {
                let query = search.text().trim().to_ascii_lowercase();
                let active_filter = filter_state.get();
                let mut visible = 0usize;

                for (haystack, status, widget) in items.borrow().iter() {
                    let matches_query = query.is_empty() || haystack.contains(&query);
                    let matches_status = active_filter.matches(status);
                    let show = matches_query && matches_status;
                    widget.set_visible(show);
                    visible += usize::from(show);
                }

                no_results.set_visible(visible == 0);
            })
        };

        {
            let apply_filters = Rc::clone(&apply_filters);
            search.connect_search_changed(move |_| apply_filters());
        }

        {
            let filter_state = Rc::clone(&filter_state);
            let apply_filters = Rc::clone(&apply_filters);
            let filter_button = filter_button.clone();
            let filter_popover = filter_popover.clone();
            all_filter.connect_toggled(move |button| {
                if !button.is_active() {
                    return;
                }
                filter_state.set(HomeFilter::All);
                filter_button.remove_css_class("filter-active");
                apply_filters();
                filter_popover.popdown();
            });
        }
        {
            let filter_state = Rc::clone(&filter_state);
            let apply_filters = Rc::clone(&apply_filters);
            let filter_button = filter_button.clone();
            let filter_popover = filter_popover.clone();
            ready_filter.connect_toggled(move |button| {
                if !button.is_active() {
                    return;
                }
                filter_state.set(HomeFilter::Ready);
                filter_button.add_css_class("filter-active");
                apply_filters();
                filter_popover.popdown();
            });
        }
        {
            let filter_state = Rc::clone(&filter_state);
            let apply_filters = Rc::clone(&apply_filters);
            let filter_button = filter_button.clone();
            let filter_popover = filter_popover.clone();
            attention_filter.connect_toggled(move |button| {
                if !button.is_active() {
                    return;
                }
                filter_state.set(HomeFilter::NeedsAttention);
                filter_button.add_css_class("filter-active");
                apply_filters();
                filter_popover.popdown();
            });
        }
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

    match &status {
        RecipeStatus::LayoutChanged => {
            let repair = action_button("wrench-symbolic", tr(language, "repair"), false);
            repair.add_css_class("repair-action");
            let weak = Rc::downgrade(state);
            let key_owned = key.to_owned();
            repair.connect_clicked(move |_| {
                let Some(state) = weak.upgrade() else {
                    return;
                };
                run_cli_with_feedback(&state, "repair", key_owned.clone());
            });
            actions.append(&repair);
        }
        RecipeStatus::Ambiguous | RecipeStatus::Offline => {}
        RecipeStatus::Ready | RecipeStatus::Missing(_) => {
            let open = action_button("media-playback-start-symbolic", tr(language, "open"), true);
            open.add_css_class("home-primary-action");
            let weak = Rc::downgrade(state);
            let key_owned = key.to_owned();
            open.connect_clicked(move |_| {
                let Some(state) = weak.upgrade() else {
                    return;
                };
                run_cli_with_feedback(&state, "open", key_owned.clone());
            });
            actions.append(&open);
        }
    }

    let edit = action_button("document-edit-symbolic", tr(language, "edit"), false);
    edit.add_css_class("quiet-action");
    let weak = Rc::downgrade(state);
    let key_owned = key.to_owned();
    edit.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            show_editor(&state, &key_owned);
        }
    });
    actions.append(&edit);

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
        "Сохранённые окружения и их текущее состояние в Niri."
    } else {
        "Saved workbenches and their current state in Niri."
    }));
    subtitle.set_xalign(0.0);
    subtitle.add_css_class("subtitle");
    page.append(&subtitle);

    let config = state.config.borrow().clone();
    let snapshot = state::niri_snapshot().ok();

    if config.workbench.is_empty() {
        let empty = gtk::Box::new(gtk::Orientation::Vertical, 8);
        empty.add_css_class("card");
        let empty_title = gtk::Label::new(Some(if language == Language::Ru {
            "Пока нет сохранённых окружений"
        } else {
            "No saved workbenches yet"
        }));
        empty_title.add_css_class("section-title");
        empty.append(&empty_title);
        let empty_hint = gtk::Label::new(Some(if language == Language::Ru {
            "Создай окружение через Capture — оно появится здесь."
        } else {
            "Create one with Capture and it will appear here."
        }));
        empty_hint.add_css_class("subtitle");
        empty.append(&empty_hint);
        page.append(&empty);
    }

    for (key, recipe) in &config.workbench {
        let status = state::recipe_status(recipe, snapshot.as_ref());
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("card");
        row.add_css_class("library-row");

        let icons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        for spec in recipe.windows.iter().take(3) {
            icons.append(&app_icon_tile(spec, 27, true));
        }
        row.append(&icons);

        let labels = gtk::Box::new(gtk::Orientation::Vertical, 4);
        labels.set_hexpand(true);
        let name = gtk::Label::new(Some(recipe_display_name(recipe)));
        name.set_xalign(0.0);
        name.add_css_class("title-lg");
        labels.append(&name);

        let detail = gtk::Label::new(Some(&format!(
            "{} · {key} · {} {}",
            recipe.workspace,
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
        labels.append(&status_badge(language, &status));
        row.append(&labels);

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 7);
        actions.set_valign(gtk::Align::Center);

        match status {
            RecipeStatus::Ready | RecipeStatus::Missing(_) => {
                let open =
                    action_button("media-playback-start-symbolic", tr(language, "open"), true);
                let weak = Rc::downgrade(state);
                let key_owned = key.clone();
                open.connect_clicked(move |_| {
                    if let Some(state) = weak.upgrade() {
                        run_cli_with_feedback(&state, "open", key_owned.clone());
                    }
                });
                actions.append(&open);
            }
            RecipeStatus::LayoutChanged => {
                let repair = action_button("wrench-symbolic", tr(language, "repair"), true);
                let weak = Rc::downgrade(state);
                let key_owned = key.clone();
                repair.connect_clicked(move |_| {
                    if let Some(state) = weak.upgrade() {
                        run_cli_with_feedback(&state, "repair", key_owned.clone());
                    }
                });
                actions.append(&repair);
            }
            RecipeStatus::Ambiguous | RecipeStatus::Offline => {}
        }

        let edit = action_button("document-edit-symbolic", tr(language, "edit"), false);
        edit.add_css_class("quiet-action");
        let weak = Rc::downgrade(state);
        let key_owned = key.clone();
        edit.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                show_editor(&state, &key_owned);
            }
        });
        actions.append(&edit);

        row.append(&actions);
        page.append(&row);
    }

    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&page)
        .build()
}

fn build_settings(state: &Rc<UiState>) -> gtk::ScrolledWindow {
    let language = state.language.get();
    let page = gtk::Box::new(gtk::Orientation::Vertical, 18);
    page.add_css_class("content");

    let title = gtk::Label::new(Some(tr(language, "settings")));
    title.add_css_class("title-xl");
    title.set_xalign(0.0);
    page.append(&title);

    let appearance = adw::PreferencesGroup::builder()
        .title(tr(language, "appearance"))
        .build();
    let language_row = adw::ActionRow::builder()
        .title(tr(language, "language"))
        .subtitle(tr(language, "language_help"))
        .build();
    let language_switch = build_language_switch(state);
    language_switch.set_valign(gtk::Align::Center);
    language_row.add_suffix(&language_switch);
    appearance.add(&language_row);
    page.append(&appearance);

    let runtime = adw::PreferencesGroup::builder()
        .title(if language == Language::Ru {
            "Niri и окружение"
        } else {
            "Niri runtime"
        })
        .build();

    match state::niri_snapshot() {
        Ok(snapshot) => {
            let connected = adw::ActionRow::builder()
                .title("Niri IPC")
                .subtitle(if language == Language::Ru {
                    "Подключено"
                } else {
                    "Connected"
                })
                .build();
            let ok = gtk::Image::from_icon_name("emblem-ok-symbolic");
            ok.add_css_class("success");
            connected.add_suffix(&ok);
            runtime.add(&connected);

            if let Some(workspace) = snapshot.focused_workspace() {
                let workspace_name = workspace
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("#{}", workspace.index));
                let workspace_row = adw::ActionRow::builder()
                    .title(if language == Language::Ru {
                        "Текущий workspace"
                    } else {
                        "Focused workspace"
                    })
                    .subtitle(workspace_name)
                    .build();
                runtime.add(&workspace_row);

                let output_row = adw::ActionRow::builder()
                    .title(if language == Language::Ru {
                        "Активный монитор"
                    } else {
                        "Focused output"
                    })
                    .subtitle(workspace.output.as_deref().unwrap_or("—"))
                    .build();
                runtime.add(&output_row);
            }

            let outputs = snapshot
                .outputs
                .iter()
                .filter(|output| output.width.is_some())
                .map(|output| match (output.width, output.height) {
                    (Some(width), Some(height)) => {
                        format!("{} · {}×{}", output.name, width, height)
                    }
                    _ => output.name.clone(),
                })
                .collect::<Vec<_>>()
                .join(", ");
            let outputs_row = adw::ActionRow::builder()
                .title(if language == Language::Ru {
                    "Подключённые мониторы"
                } else {
                    "Connected outputs"
                })
                .subtitle(if outputs.is_empty() { "—" } else { &outputs })
                .subtitle_lines(2)
                .build();
            runtime.add(&outputs_row);
        }
        Err(error) => {
            let offline = adw::ActionRow::builder()
                .title("Niri IPC")
                .subtitle(if language == Language::Ru {
                    format!("Недоступен: {error}")
                } else {
                    format!("Unavailable: {error}")
                })
                .subtitle_lines(2)
                .build();
            offline.add_css_class("error");
            runtime.add(&offline);
        }
    }

    if let Ok(socket) = state::discover_socket() {
        let socket_row = adw::ActionRow::builder()
            .title("NIRI_SOCKET")
            .subtitle(socket.display().to_string())
            .subtitle_lines(2)
            .build();
        runtime.add(&socket_row);
    }

    let refresh_runtime = action_button("view-refresh-symbolic", tr(language, "refresh"), false);
    refresh_runtime.set_valign(gtk::Align::Center);
    let refresh_row = adw::ActionRow::builder()
        .title(if language == Language::Ru {
            "Обновить состояние"
        } else {
            "Refresh runtime"
        })
        .subtitle(if language == Language::Ru {
            "Повторно опросить Niri и мониторы"
        } else {
            "Query Niri and connected outputs again"
        })
        .build();
    refresh_row.add_suffix(&refresh_runtime);
    refresh_row.set_activatable_widget(Some(&refresh_runtime));
    let weak = Rc::downgrade(state);
    refresh_runtime.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            show_settings(&state);
        }
    });
    runtime.add(&refresh_row);
    page.append(&runtime);

    let config_group = adw::PreferencesGroup::builder()
        .title(if language == Language::Ru {
            "Конфигурация"
        } else {
            "Configuration"
        })
        .build();

    let config_path = adw::ActionRow::builder()
        .title(if language == Language::Ru {
            "Файл конфигурации"
        } else {
            "Configuration file"
        })
        .subtitle(state.config_path.display().to_string())
        .subtitle_lines(2)
        .build();
    config_group.add(&config_path);

    let count = state.config.borrow().workbench.len();
    let count_row = adw::ActionRow::builder()
        .title(if language == Language::Ru {
            "Сохранённые окружения"
        } else {
            "Saved workbenches"
        })
        .subtitle(count.to_string())
        .build();
    config_group.add(&count_row);

    if let Some(error) = state.load_error.borrow().as_deref() {
        let error_row = adw::ActionRow::builder()
            .title(if language == Language::Ru {
                "Ошибка конфигурации"
            } else {
                "Configuration error"
            })
            .subtitle(error)
            .subtitle_lines(3)
            .build();
        error_row.add_css_class("error");
        config_group.add(&error_row);
    }

    let reload = action_button(
        "view-refresh-symbolic",
        if language == Language::Ru {
            "Перечитать"
        } else {
            "Reload"
        },
        false,
    );
    reload.set_valign(gtk::Align::Center);
    let reload_row = adw::ActionRow::builder()
        .title(if language == Language::Ru {
            "Перечитать файл"
        } else {
            "Reload configuration"
        })
        .subtitle(if language == Language::Ru {
            "Загрузить изменения из TOML без перезапуска Workbench"
        } else {
            "Load TOML changes without restarting Workbench"
        })
        .build();
    reload_row.add_suffix(&reload);
    reload_row.set_activatable_widget(Some(&reload));
    let weak = Rc::downgrade(state);
    reload.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            reload_config_from_disk(&state, true);
            show_settings(&state);
        }
    });
    config_group.add(&reload_row);
    page.append(&config_group);

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

    if spec.command.is_empty() {
        return if language == Language::Ru {
            "Только переиспользовать это окно"
        } else {
            "Reuse this window only"
        };
    }

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
    } else if app.contains("chrome") || app.contains("chromium") {
        let has_profile = spec
            .command
            .iter()
            .any(|part| part.starts_with("--profile-directory="));
        let has_url = spec
            .command
            .iter()
            .any(|part| part.starts_with("http://") || part.starts_with("https://"));
        match (language, has_profile, has_url) {
            (Language::Ru, true, true) => "Тот же профиль + активная страница",
            (Language::Ru, true, false) => "Открыть тот же профиль Chrome",
            (Language::Ru, false, true) => "Переоткрыть активную страницу",
            (Language::Ru, false, false) => "Открыть Chrome",
            (_, true, true) => "Same profile + active page",
            (_, true, false) => "Open the same Chrome profile",
            (_, false, true) => "Reopen the active page",
            (_, false, false) => "Open Chrome",
        }
    } else if app.contains("firefox") || app.contains("browser") {
        if language == Language::Ru {
            "Открыть браузер"
        } else {
            "Open browser"
        }
    } else if language == Language::Ru {
        "Запустить установленное приложение"
    } else {
        "Launch installed application"
    }
}

fn output_dropdown(
    language: Language,
    current: Option<&str>,
    connected: &[String],
) -> (gtk::DropDown, Rc<Vec<Option<String>>>) {
    let mut names = connected.to_vec();
    if let Some(current) = current {
        if !current.is_empty() && !names.iter().any(|name| name == current) {
            names.push(current.to_owned());
        }
    }
    names.sort();
    names.dedup();

    let mut options = vec![None];
    options.extend(names.into_iter().map(Some));
    let labels = options
        .iter()
        .map(|value| {
            value
                .clone()
                .unwrap_or_else(|| tr(language, "auto").to_owned())
        })
        .collect::<Vec<_>>();
    let refs = labels.iter().map(String::as_str).collect::<Vec<_>>();
    let dropdown = gtk::DropDown::from_strings(&refs);
    let selected = options
        .iter()
        .position(|value| value.as_deref() == current)
        .unwrap_or(0);
    dropdown.set_selected(selected as u32);

    (dropdown, Rc::new(options))
}

fn toast_message(state: &Rc<UiState>, message: &str) {
    state.toast.add_toast(adw::Toast::new(message));
}

fn set_entry_validation(
    entry: &gtk::Entry,
    errors: &ValidationErrors,
    key: &str,
    error: Option<String>,
) {
    if let Some(error) = error {
        errors.borrow_mut().insert(key.to_owned(), error.clone());
        entry.add_css_class("validation-error");
        entry.set_tooltip_text(Some(&error));
    } else {
        errors.borrow_mut().remove(key);
        entry.remove_css_class("validation-error");
        entry.set_tooltip_text(None);
    }
}

fn refresh_capture_create_state(
    workspace: &gtk::Entry,
    errors: &ValidationErrors,
    create: &gtk::Button,
) {
    create.set_sensitive(!workspace.text().trim().is_empty() && errors.borrow().is_empty());
}

fn cli_error_summary(text: &str) -> String {
    text.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(text)
        .trim()
        .chars()
        .take(180)
        .collect()
}

fn run_cli_with_feedback(state: &Rc<UiState>, action: &'static str, key: String) {
    let language = state.language.get();
    let pending = match (language, action) {
        (Language::Ru, "open") => "Открываю окружение…",
        (Language::Ru, "repair") => "Восстанавливаю раскладку…",
        (_, "open") => "Opening workbench…",
        (_, "repair") => "Restoring layout…",
        _ => "Working…",
    };
    toast_message(state, pending);

    let slot: Arc<Mutex<Option<Result<state::CliRunResult, String>>>> = Arc::new(Mutex::new(None));
    let slot_worker = Arc::clone(&slot);
    let config_path = state.config_path.clone();
    let key_worker = key.clone();

    std::thread::spawn(move || {
        let result =
            state::run_cli(action, &key_worker, &config_path).map_err(|error| error.to_string());
        if let Ok(mut guard) = slot_worker.lock() {
            *guard = Some(result);
        }
    });

    let weak = Rc::downgrade(state);
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let result = slot.lock().ok().and_then(|mut guard| guard.take());
        let Some(result) = result else {
            return glib::ControlFlow::Continue;
        };

        if let Some(state) = weak.upgrade() {
            match result {
                Ok(result) if result.success => {
                    let message = match (state.language.get(), action) {
                        (Language::Ru, "open") => "Окружение открыто",
                        (Language::Ru, "repair") => "Раскладка восстановлена",
                        (_, "open") => "Workbench opened",
                        (_, "repair") => "Layout restored",
                        _ => "Done",
                    };
                    toast_message(&state, message);
                    show_home(&state);
                }
                Ok(result) => {
                    let summary = cli_error_summary(&result.output);
                    let message = if state.language.get() == Language::Ru {
                        format!("Не удалось: {summary}")
                    } else {
                        format!("Failed: {summary}")
                    };
                    toast_message(&state, &message);
                }
                Err(error) => {
                    let message = if state.language.get() == Language::Ru {
                        format!("Ошибка запуска: {error}")
                    } else {
                        format!("Launch error: {error}")
                    };
                    toast_message(&state, &message);
                }
            }
        }

        glib::ControlFlow::Break
    });
}

fn run_quick_cli(
    window: &adw::ApplicationWindow,
    toast: &adw::ToastOverlay,
    action: &'static str,
    key: String,
    config_path: std::path::PathBuf,
    language: Language,
) {
    let pending = match (language, action) {
        (Language::Ru, "open") => "Открываю окружение…",
        (Language::Ru, "repair") => "Восстанавливаю раскладку…",
        (_, "open") => "Opening workbench…",
        (_, "repair") => "Restoring layout…",
        _ => "Working…",
    };
    toast.add_toast(adw::Toast::new(pending));

    let slot: Arc<Mutex<Option<Result<state::CliRunResult, String>>>> = Arc::new(Mutex::new(None));
    let worker = Arc::clone(&slot);
    std::thread::spawn(move || {
        let result = state::run_cli(action, &key, &config_path).map_err(|error| error.to_string());
        if let Ok(mut guard) = worker.lock() {
            *guard = Some(result);
        }
    });

    let window = window.clone();
    let toast = toast.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let result = slot.lock().ok().and_then(|mut guard| guard.take());
        let Some(result) = result else {
            return glib::ControlFlow::Continue;
        };

        match result {
            Ok(result) if result.success => {
                window.close();
            }
            Ok(result) => {
                let summary = cli_error_summary(&result.output);
                let message = if language == Language::Ru {
                    format!("Не удалось: {summary}")
                } else {
                    format!("Failed: {summary}")
                };
                toast.add_toast(adw::Toast::new(&message));
            }
            Err(error) => {
                let message = if language == Language::Ru {
                    format!("Ошибка запуска: {error}")
                } else {
                    format!("Launch error: {error}")
                };
                toast.add_toast(adw::Toast::new(&message));
            }
        }

        glib::ControlFlow::Break
    });
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
    let capture_errors: ValidationErrors = Rc::new(RefCell::new(HashMap::new()));

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
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.add_css_class("card");
            row.add_css_class("capture-window-row");

            row.append(&app_icon_tile(spec, 31, false));

            let labels = gtk::Box::new(gtk::Orientation::Vertical, 3);
            labels.set_width_request(220);
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

            let launch = gtk::Box::new(gtk::Orientation::Vertical, 6);
            launch.set_hexpand(true);

            let behavior = gtk::DropDown::from_strings(&[
                if language == Language::Ru {
                    "Переиспользовать подходящее окно"
                } else {
                    "Reuse matching window"
                },
                if language == Language::Ru {
                    "Всегда запускать новое"
                } else {
                    "Always launch new"
                },
            ]);
            behavior.set_selected(match spec.reuse {
                ReusePolicy::Unique => 0,
                ReusePolicy::Never => 1,
            });
            behavior.set_tooltip_text(Some(capture_behavior_text(spec, language)));
            {
                let draft = Rc::clone(&draft);
                behavior.connect_selected_notify(move |dropdown| {
                    if let Some(window) = draft.borrow_mut().recipe.windows.get_mut(index) {
                        window.reuse = if dropdown.selected() == 0 {
                            ReusePolicy::Unique
                        } else {
                            ReusePolicy::Never
                        };
                    }
                });
            }
            launch.append(&behavior);

            let command = gtk::Entry::new();
            command.set_width_chars(24);
            command.set_hexpand(true);
            let command_text = state::display_command(&spec.command);
            command.set_text(&command_text);
            command.set_placeholder_text(Some(tr(language, "reuse_only")));
            command.set_tooltip_text(Some(tr(language, "launch_command")));
            launch.append(&command);
            row.append(&launch);

            command_entries.borrow_mut().push((index, command));
            windows_box.append(&row);
        }
    }
    page.append(&windows_box);

    let lower = gtk::Grid::new();
    lower.set_column_spacing(14);
    lower.set_column_homogeneous(true);

    let details_card = gtk::Box::new(gtk::Orientation::Vertical, 11);
    details_card.add_css_class("card");
    details_card.set_hexpand(true);

    let details_header = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let section = gtk::Label::new(Some(tr(language, "workbench_details")));
    section.add_css_class("section-title");
    details_header.append(&section);
    details_card.append(&details_header);

    let name_entry = gtk::Entry::new();
    name_entry.set_text(recipe_display_name(&draft.borrow().recipe));
    details_card.append(&capture_labeled_control(tr(language, "name"), &name_entry));

    let workspace_entry = gtk::Entry::new();
    workspace_entry.set_text(&draft.borrow().recipe.workspace);
    details_card.append(&capture_labeled_control(
        tr(language, "workspace_name"),
        &workspace_entry,
    ));

    {
        let errors = Rc::clone(&capture_errors);
        let create = create.clone();
        workspace_entry.connect_changed(move |entry| {
            let error = entry.text().trim().is_empty().then(|| {
                if language == Language::Ru {
                    "Имя workspace не может быть пустым".to_owned()
                } else {
                    "Workspace name cannot be empty".to_owned()
                }
            });
            set_entry_validation(entry, &errors, "workspace", error);
            refresh_capture_create_state(entry, &errors, &create);
        });
    }

    for (index, entry) in command_entries.borrow().iter() {
        let errors = Rc::clone(&capture_errors);
        let workspace = workspace_entry.clone();
        let create = create.clone();
        let key = format!("command-{index}");
        entry.connect_changed(move |entry| {
            let error = state::parse_command(entry.text().as_str())
                .err()
                .map(|error| error.to_string());
            set_entry_validation(entry, &errors, &key, error);
            refresh_capture_create_state(&workspace, &errors, &create);
        });
    }
    refresh_capture_create_state(&workspace_entry, &capture_errors, &create);

    let connected_outputs = snapshot
        .outputs
        .iter()
        .filter(|output| output.width.is_some())
        .map(|output| output.name.clone())
        .collect::<Vec<_>>();
    let current_output = draft.borrow().recipe.output.clone();
    let (output, output_options) =
        output_dropdown(language, current_output.as_deref(), &connected_outputs);
    {
        let draft = Rc::clone(&draft);
        output.connect_selected_notify(move |dropdown| {
            let selected = usize::try_from(dropdown.selected()).unwrap_or(0);
            draft.borrow_mut().recipe.output = output_options.get(selected).cloned().flatten();
        });
    }
    details_card.append(&capture_labeled_control(tr(language, "output"), &output));

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
    lower.attach(&details_card, 0, 0, 1, 1);

    let preview_card = gtk::Box::new(gtk::Orientation::Vertical, 10);
    preview_card.add_css_class("card");
    preview_card.set_hexpand(true);

    let preview_header = gtk::Box::new(gtk::Orientation::Horizontal, 10);
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
    preview_card.append(&build_capture_preview(&draft.borrow().recipe));
    lower.attach(&preview_card, 1, 0, 1, 1);

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
            toast_message(
                &state,
                if state.language.get() == Language::Ru {
                    "Имя workspace не может быть пустым"
                } else {
                    "Workspace name cannot be empty"
                },
            );
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

fn capture_labeled_control(label_text: &str, control: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let label = gtk::Label::new(Some(label_text));
    label.set_width_chars(10);
    label.set_xalign(0.0);
    row.append(&label);
    control.set_hexpand(true);
    row.append(control);
    row
}

fn build_capture_preview(recipe: &Recipe) -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.add_css_class("layout-canvas");
    outer.add_css_class("capture-preview");
    outer.set_height_request(190);
    outer.set_hexpand(true);
    outer.set_vexpand(true);

    let overlay = gtk::Overlay::new();
    overlay.set_hexpand(true);
    overlay.set_vexpand(true);

    let grid = gtk::Grid::new();
    grid.set_column_homogeneous(true);
    grid.set_row_homogeneous(true);
    grid.set_column_spacing(6);
    grid.set_row_spacing(6);
    grid.set_hexpand(true);
    grid.set_vexpand(true);

    let mut columns: BTreeMap<usize, Vec<&WindowSpec>> = BTreeMap::new();
    let mut floating = Vec::new();
    for spec in &recipe.windows {
        if spec.layout.floating {
            floating.push(spec);
        } else {
            columns.entry(spec.layout.column).or_default().push(spec);
        }
    }

    if columns.is_empty() {
        let empty = gtk::Label::new(Some("—"));
        empty.add_css_class("subtitle");
        grid.attach(&empty, 0, 0, 20, 20);
    } else {
        let fractions = columns
            .values()
            .map(|windows| {
                windows
                    .iter()
                    .find_map(|window| match window.layout.column_width {
                        Some(Size::Percent(value)) if value.is_finite() && value > 0.0 => {
                            Some(value)
                        }
                        _ => None,
                    })
                    .unwrap_or(1.0)
            })
            .collect::<Vec<_>>();
        let fraction_sum = fractions.iter().sum::<f64>().max(f64::EPSILON);
        let total_columns = ((columns.len() as i32) * 4).max(20);

        let mut column_start = 0_i32;
        let column_count = columns.len();
        for (column_index, windows) in columns.values().enumerate() {
            let columns_left = column_count - column_index;
            let remaining_units = total_columns - column_start;
            let column_span = if columns_left == 1 {
                remaining_units
            } else {
                let desired = ((fractions[column_index] / fraction_sum) * f64::from(total_columns))
                    .round() as i32;
                let max_span = remaining_units - ((columns_left as i32 - 1) * 2);
                desired.clamp(2, max_span)
            };

            let heights = windows
                .iter()
                .map(|window| match window.layout.window_height {
                    Some(Size::Percent(value)) if value.is_finite() && value > 0.0 => value,
                    _ => 1.0,
                })
                .collect::<Vec<_>>();
            let height_sum = heights.iter().sum::<f64>().max(f64::EPSILON);
            let total_rows = ((windows.len() as i32) * 4).max(20);
            let mut row_start = 0_i32;

            for (window_index, spec) in windows.iter().enumerate() {
                let windows_left = windows.len() - window_index;
                let remaining_rows = total_rows - row_start;
                let row_span = if windows_left == 1 {
                    remaining_rows
                } else {
                    let desired = ((heights[window_index] / height_sum) * f64::from(total_rows))
                        .round() as i32;
                    let max_span = remaining_rows - ((windows_left as i32 - 1) * 2);
                    desired.clamp(2, max_span)
                };

                let tile = gtk::Box::new(gtk::Orientation::Vertical, 4);
                tile.add_css_class("capture-preview-window");
                tile.set_hexpand(true);
                tile.set_vexpand(true);
                tile.set_halign(gtk::Align::Fill);
                tile.set_valign(gtk::Align::Fill);

                let icon_name = state::icon_name(spec);
                let icon = gtk::Image::from_icon_name(&icon_name);
                icon.set_pixel_size(24);
                icon.set_halign(gtk::Align::Center);
                tile.append(&icon);

                let label = gtk::Label::new(Some(&friendly_window_name(spec)));
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                label.set_max_width_chars(14);
                label.set_halign(gtk::Align::Center);
                label.add_css_class("capture-preview-label");
                tile.append(&label);

                grid.attach(&tile, column_start, row_start, column_span, row_span);
                row_start += row_span;
            }

            column_start += column_span;
        }
    }

    overlay.set_child(Some(&grid));

    for (index, spec) in floating.into_iter().take(2).enumerate() {
        let badge = gtk::Label::new(Some(&format!("◫ {}", friendly_window_name(spec))));
        badge.add_css_class("capture-floating-window");
        badge.set_halign(gtk::Align::End);
        badge.set_valign(gtk::Align::Start);
        badge.set_margin_top(8 + (index as i32 * 30));
        badge.set_margin_end(8);
        overlay.add_overlay(&badge);
    }

    outer.append(&overlay);
    outer
}

fn build_layout_canvas(
    recipe: &Recipe,
    language: Language,
    selected: Option<&str>,
    on_select: Option<Rc<dyn Fn(String)>>,
    on_drop: Option<DropCallback>,
    on_new_column: Option<SimpleDropCallback>,
    on_float: Option<SimpleDropCallback>,
) -> gtk::Box {
    let canvas = gtk::Box::new(gtk::Orientation::Vertical, 8);
    canvas.add_css_class("layout-canvas");

    let stage = gtk::Overlay::new();
    stage.set_height_request(300);
    stage.set_hexpand(true);
    stage.add_css_class("layout-stage");

    let grid = gtk::Grid::new();
    grid.set_column_homogeneous(true);
    grid.set_row_homogeneous(true);
    grid.set_column_spacing(8);
    grid.set_row_spacing(8);
    grid.set_hexpand(true);
    grid.set_vexpand(true);

    let mut columns: BTreeMap<usize, Vec<&WindowSpec>> = BTreeMap::new();
    let mut floating = Vec::new();
    for spec in &recipe.windows {
        if spec.layout.floating {
            floating.push(spec);
        } else {
            columns.entry(spec.layout.column).or_default().push(spec);
        }
    }

    let column_fractions = columns
        .values()
        .map(|windows| {
            windows
                .iter()
                .find_map(|window| match window.layout.column_width {
                    Some(Size::Percent(value)) if value.is_finite() && value > 0.0 => Some(value),
                    _ => None,
                })
                .unwrap_or(1.0)
        })
        .collect::<Vec<_>>();
    let column_sum = column_fractions.iter().sum::<f64>().max(f64::EPSILON);
    let total_columns = ((columns.len() as i32) * 6).max(24);
    let drag_enabled = on_drop.is_some() || on_new_column.is_some() || on_float.is_some();

    let mut column_start = 0_i32;
    let column_count = columns.len();
    for (column_index, windows) in columns.values().enumerate() {
        let columns_left = column_count - column_index;
        let remaining_columns = total_columns - column_start;
        let column_span = if columns_left == 1 {
            remaining_columns
        } else {
            let wanted = ((column_fractions[column_index] / column_sum) * f64::from(total_columns))
                .round() as i32;
            let max_span = remaining_columns - ((columns_left as i32 - 1) * 3);
            wanted.clamp(3, max_span)
        };

        let row_fractions = windows
            .iter()
            .map(|window| match window.layout.window_height {
                Some(Size::Percent(value)) if value.is_finite() && value > 0.0 => value,
                _ => 1.0,
            })
            .collect::<Vec<_>>();
        let row_sum = row_fractions.iter().sum::<f64>().max(f64::EPSILON);
        let total_rows = ((windows.len() as i32) * 6).max(24);
        let mut row_start = 0_i32;

        for (window_index, spec) in windows.iter().enumerate() {
            let windows_left = windows.len() - window_index;
            let remaining_rows = total_rows - row_start;
            let row_span = if windows_left == 1 {
                remaining_rows
            } else {
                let wanted = ((row_fractions[window_index] / row_sum) * f64::from(total_rows))
                    .round() as i32;
                let max_span = remaining_rows - ((windows_left as i32 - 1) * 3);
                wanted.clamp(3, max_span)
            };

            let button = gtk::Button::new();
            button.add_css_class("layout-window");
            if matches!(spec.layout.display, Some(ColumnDisplay::Tabbed)) {
                button.add_css_class("layout-tabbed-window");
            }
            if selected == Some(spec.name.as_str()) {
                button.add_css_class("layout-window-selected");
            }
            button.set_hexpand(true);
            button.set_vexpand(true);

            let overlay = gtk::Overlay::new();
            let inside = gtk::Box::new(gtk::Orientation::Vertical, 5);
            inside.set_valign(gtk::Align::Center);
            inside.set_halign(gtk::Align::Center);

            let icon_name = state::icon_name(spec);
            let icon = gtk::Image::from_icon_name(&icon_name);
            icon.set_pixel_size(36);
            inside.append(&icon);

            let title = gtk::Label::new(Some(&friendly_window_name(spec)));
            title.add_css_class("section-title");
            title.set_ellipsize(gtk::pango::EllipsizeMode::End);
            inside.append(&title);

            let logical = gtk::Label::new(Some(&spec.name));
            logical.add_css_class("subtitle");
            logical.set_ellipsize(gtk::pango::EllipsizeMode::End);
            inside.append(&logical);
            overlay.set_child(Some(&inside));

            let handle = gtk::Image::from_icon_name("list-drag-handle-symbolic");
            handle.set_pixel_size(14);
            handle.set_halign(gtk::Align::Start);
            handle.set_valign(gtk::Align::Start);
            handle.set_margin_top(7);
            handle.set_margin_start(7);
            handle.add_css_class("dim");
            overlay.add_overlay(&handle);

            if matches!(spec.layout.display, Some(ColumnDisplay::Tabbed)) {
                let badge = gtk::Label::new(Some(if language == Language::Ru {
                    "ВКЛАДКИ"
                } else {
                    "TABBED"
                }));
                badge.add_css_class("layout-mode-badge");
                badge.set_halign(gtk::Align::End);
                badge.set_valign(gtk::Align::Start);
                badge.set_margin_top(7);
                badge.set_margin_end(7);
                overlay.add_overlay(&badge);
            }

            button.set_child(Some(&overlay));

            if let Some(callback) = on_select.clone() {
                let name = spec.name.clone();
                button.connect_clicked(move |_| callback(name.clone()));
            }

            if drag_enabled {
                let source = gtk::DragSource::new();
                source.set_actions(gtk::gdk::DragAction::MOVE);
                let drag_name = spec.name.clone();
                source.connect_prepare(move |_, _, _| {
                    Some(gtk::gdk::ContentProvider::for_value(&drag_name.to_value()))
                });
                button.add_controller(source);
            }

            if let Some(callback) = on_drop.clone() {
                let target = gtk::DropTarget::new(glib::Type::STRING, gtk::gdk::DragAction::MOVE);
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
                button.add_controller(target);
            }

            grid.attach(&button, column_start, row_start, column_span, row_span);
            row_start += row_span;
        }

        column_start += column_span;
    }

    stage.set_child(Some(&grid));

    if !floating.is_empty() {
        let floating_stack = gtk::Box::new(gtk::Orientation::Vertical, 6);
        floating_stack.add_css_class("floating-stack");
        floating_stack.set_halign(gtk::Align::End);
        floating_stack.set_valign(gtk::Align::Start);
        floating_stack.set_margin_top(10);
        floating_stack.set_margin_end(10);

        for spec in floating {
            let button = gtk::Button::new();
            button.add_css_class("floating-layout-window");
            if selected == Some(spec.name.as_str()) {
                button.add_css_class("layout-window-selected");
            }

            let row = gtk::Box::new(gtk::Orientation::Horizontal, 7);
            row.append(&app_icon_tile(spec, 22, true));
            let label = gtk::Label::new(Some(&friendly_window_name(spec)));
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            row.append(&label);
            button.set_child(Some(&row));

            if let Some(callback) = on_select.clone() {
                let name = spec.name.clone();
                button.connect_clicked(move |_| callback(name.clone()));
            }

            if drag_enabled {
                let source = gtk::DragSource::new();
                source.set_actions(gtk::gdk::DragAction::MOVE);
                let drag_name = spec.name.clone();
                source.connect_prepare(move |_, _, _| {
                    Some(gtk::gdk::ContentProvider::for_value(&drag_name.to_value()))
                });
                button.add_controller(source);
            }

            floating_stack.append(&button);
        }

        stage.add_overlay(&floating_stack);
    }

    canvas.append(&stage);

    if on_new_column.is_some() || on_float.is_some() {
        let drop_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        drop_bar.add_css_class("layout-drop-bar");

        if let Some(callback) = on_new_column {
            let zone = gtk::Box::new(gtk::Orientation::Horizontal, 7);
            zone.add_css_class("layout-drop-zone");
            zone.set_hexpand(true);
            let icon = gtk::Image::from_icon_name("list-add-symbolic");
            icon.set_pixel_size(16);
            zone.append(&icon);
            let label = gtk::Label::new(Some(if language == Language::Ru {
                "Новая колонка"
            } else {
                "New column"
            }));
            label.add_css_class("layout-drop-label");
            zone.append(&label);

            let target = gtk::DropTarget::new(glib::Type::STRING, gtk::gdk::DragAction::MOVE);
            target.connect_drop(move |_, value, _, _| {
                let Ok(dragged) = value.get::<String>() else {
                    return false;
                };
                callback(dragged);
                true
            });
            zone.add_controller(target);
            drop_bar.append(&zone);
        }

        if let Some(callback) = on_float {
            let zone = gtk::Box::new(gtk::Orientation::Horizontal, 7);
            zone.add_css_class("layout-drop-zone");
            zone.set_hexpand(true);
            let icon = gtk::Image::from_icon_name("view-restore-symbolic");
            icon.set_pixel_size(16);
            zone.append(&icon);
            let label = gtk::Label::new(Some(if language == Language::Ru {
                "Плавающее окно"
            } else {
                "Floating"
            }));
            label.add_css_class("layout-drop-label");
            zone.append(&label);

            let target = gtk::DropTarget::new(glib::Type::STRING, gtk::gdk::DragAction::MOVE);
            target.connect_drop(move |_, value, _, _| {
                let Ok(dragged) = value.get::<String>() else {
                    return false;
                };
                callback(dragged);
                true
            });
            zone.add_controller(target);
            drop_bar.append(&zone);
        }

        canvas.append(&drop_bar);
    }

    canvas
}

fn save_editor_draft(
    state: &Rc<UiState>,
    key: &str,
    draft: Rc<RefCell<Recipe>>,
    validation_errors: &Rc<RefCell<HashMap<String, String>>>,
    selected: Option<String>,
) {
    if let Some(error) = validation_errors.borrow().values().next().cloned() {
        let message = if state.language.get() == Language::Ru {
            format!("Исправь ошибку перед сохранением: {error}")
        } else {
            format!("Fix the invalid field before saving: {error}")
        };
        toast_message(state, &message);
        return;
    }

    if !editor_has_unsaved_changes(state, key, &draft.borrow()) {
        return;
    }

    let mut recipe = draft.borrow().clone();
    state::normalize_columns(&mut recipe);

    let mut next = state.config.borrow().clone();
    next.workbench.insert(key.to_owned(), recipe.clone());

    match state::persist_config(&state.config_path, &next) {
        Ok(()) => {
            *state.config.borrow_mut() = next;
            *draft.borrow_mut() = recipe;
            toast_message(state, tr(state.language.get(), "saved"));
            show_editor_session(state, key, Rc::clone(&draft), selected);
        }
        Err(error) => toast_message(state, &error.to_string()),
    }
}

fn editor_has_unsaved_changes(state: &Rc<UiState>, key: &str, draft: &Recipe) -> bool {
    state
        .config
        .borrow()
        .workbench
        .get(key)
        .is_none_or(|saved| saved != draft)
}

fn refresh_editor_status(
    state: &Rc<UiState>,
    key: &str,
    draft: &Rc<RefCell<Recipe>>,
    validation_errors: &Rc<RefCell<HashMap<String, String>>>,
    status: &gtk::Label,
    save: &gtk::Button,
) {
    let has_errors = !validation_errors.borrow().is_empty();
    let dirty = editor_has_unsaved_changes(state, key, &draft.borrow());

    status.remove_css_class("status-ready");
    status.remove_css_class("status-warning");
    status.remove_css_class("status-error");

    if has_errors {
        status.set_text(if state.language.get() == Language::Ru {
            "Исправь ошибки"
        } else {
            "Fix errors"
        });
        status.add_css_class("status-error");
        save.set_sensitive(false);
    } else if dirty {
        status.set_text(if state.language.get() == Language::Ru {
            "Есть изменения"
        } else {
            "Unsaved changes"
        });
        status.add_css_class("status-warning");
        save.set_sensitive(true);
    } else {
        status.set_text(if state.language.get() == Language::Ru {
            "Сохранено"
        } else {
            "Saved"
        });
        status.add_css_class("status-ready");
        save.set_sensitive(false);
    }
}

fn show_discard_confirmation(
    state: &Rc<UiState>,
    message: &str,
    detail: &str,
    on_discard: StateAction,
) {
    let language = state.language.get();
    let keep_editing = if language == Language::Ru {
        "Продолжить"
    } else {
        "Keep"
    };
    let discard = if language == Language::Ru {
        "Не сохранять"
    } else {
        "Discard"
    };
    let dialog = gtk::AlertDialog::builder()
        .modal(true)
        .message(message)
        .detail(detail)
        .build();
    dialog.set_buttons(&[keep_editing, discard]);
    dialog.set_cancel_button(0);
    dialog.set_default_button(0);

    let weak = Rc::downgrade(state);
    dialog.choose(
        Some(&state.window),
        None::<&gtk::gio::Cancellable>,
        move |response| {
            if matches!(response, Ok(1)) {
                if let Some(state) = weak.upgrade() {
                    on_discard(&state);
                }
            }
        },
    );
}

fn editor_requires_confirmation(
    state: &Rc<UiState>,
    key: &str,
    draft: &Rc<RefCell<Recipe>>,
    validation_errors: &ValidationErrors,
) -> bool {
    editor_has_unsaved_changes(state, key, &draft.borrow())
        || !validation_errors.borrow().is_empty()
}

fn request_leave_editor(
    state: &Rc<UiState>,
    key: &str,
    draft: Rc<RefCell<Recipe>>,
    validation_errors: ValidationErrors,
) {
    if !editor_requires_confirmation(state, key, &draft, &validation_errors) {
        show_library(state);
        return;
    }

    let (message, detail) = if state.language.get() == Language::Ru {
        (
            "Не сохранять изменения?",
            "Изменения этого окружения ещё не сохранены.",
        )
    } else {
        (
            "Discard changes?",
            "Changes to this workbench have not been saved yet.",
        )
    };
    let on_discard: StateAction = Rc::new(show_library);
    show_discard_confirmation(state, message, detail, on_discard);
}

fn show_editor(state: &Rc<UiState>, key: &str) {
    let Some(recipe) = state.config.borrow().workbench.get(key).cloned() else {
        toast_message(
            state,
            if state.language.get() == Language::Ru {
                "Окружение не найдено"
            } else {
                "Unknown workbench"
            },
        );
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
    let validation_errors: ValidationErrors = Rc::new(RefCell::new(HashMap::new()));
    {
        let borrowed = draft.borrow();
        if borrowed.workspace.trim().is_empty() {
            validation_errors.borrow_mut().insert(
                "workspace".to_owned(),
                if language == Language::Ru {
                    "Имя workspace не может быть пустым".to_owned()
                } else {
                    "Workspace name cannot be empty".to_owned()
                },
            );
        }
        if borrowed.windows.is_empty() {
            validation_errors.borrow_mut().insert(
                "windows".to_owned(),
                if language == Language::Ru {
                    "В окружении должно быть хотя бы одно окно".to_owned()
                } else {
                    "The workbench must contain at least one window".to_owned()
                },
            );
        }
    }
    *state.editor_guard.borrow_mut() = Some(EditorGuard {
        key: key.to_owned(),
        draft: Rc::clone(&draft),
        validation_errors: Rc::clone(&validation_errors),
    });
    let page = gtk::Box::new(gtk::Orientation::Vertical, 14);
    page.add_css_class("content");

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let back = icon_button("go-previous-symbolic", "back-button");
    back.set_tooltip_text(Some(if language == Language::Ru {
        "Назад · Alt+←"
    } else {
        "Back · Alt+←"
    }));
    let weak = Rc::downgrade(state);
    let key_for_back = key.to_owned();
    let draft_for_back = Rc::clone(&draft);
    let validation_for_back = Rc::clone(&validation_errors);
    back.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            request_leave_editor(
                &state,
                &key_for_back,
                Rc::clone(&draft_for_back),
                Rc::clone(&validation_for_back),
            );
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

    let editor_status = gtk::Label::new(None);
    editor_status.add_css_class("status-pill");
    editor_status.set_valign(gtk::Align::Center);
    header.append(&editor_status);

    let preview = action_button("view-reveal-symbolic", tr(language, "preview"), false);
    preview.add_css_class("quiet-action");
    {
        let state = Rc::clone(state);
        let draft = Rc::clone(&draft);
        preview.connect_clicked(move |_| {
            show_layout_preview(&state, &draft.borrow());
        });
    }
    header.append(&preview);

    let cancel = action_button("window-close-symbolic", tr(language, "cancel"), false);
    cancel.add_css_class("quiet-action");
    let weak = Rc::downgrade(state);
    let key_for_cancel = key.to_owned();
    let draft_for_cancel = Rc::clone(&draft);
    let validation_for_cancel = Rc::clone(&validation_errors);
    cancel.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            request_leave_editor(
                &state,
                &key_for_cancel,
                Rc::clone(&draft_for_cancel),
                Rc::clone(&validation_for_cancel),
            );
        }
    });
    header.append(&cancel);

    let save = action_button("object-select-symbolic", tr(language, "save"), true);
    save.set_tooltip_text(Some(if language == Language::Ru {
        "Сохранить · Ctrl+S"
    } else {
        "Save · Ctrl+S"
    }));
    header.append(&save);

    refresh_editor_status(
        state,
        key,
        &draft,
        &validation_errors,
        &editor_status,
        &save,
    );
    {
        let weak_state = Rc::downgrade(state);
        let key = key.to_owned();
        let draft = Rc::clone(&draft);
        let validation_errors = Rc::clone(&validation_errors);
        let weak_status = editor_status.downgrade();
        let weak_save = save.downgrade();
        glib::timeout_add_local(Duration::from_millis(120), move || {
            let Some(state) = weak_state.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let Some(status) = weak_status.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let Some(save) = weak_save.upgrade() else {
                return glib::ControlFlow::Break;
            };

            refresh_editor_status(&state, &key, &draft, &validation_errors, &status, &save);
            glib::ControlFlow::Continue
        });
    }

    page.append(&header);

    let main = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    main.set_hexpand(true);

    let left = gtk::Box::new(gtk::Orientation::Vertical, 12);
    left.set_hexpand(true);

    let layout_panel = gtk::Box::new(gtk::Orientation::Vertical, 10);
    layout_panel.add_css_class("card");
    layout_panel.add_css_class("editor-layout-card");

    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let layout_title = gtk::Label::new(Some(tr(language, "layout")));
    layout_title.add_css_class("section-title");
    layout_title.set_xalign(0.0);
    layout_title.set_hexpand(true);
    toolbar.append(&layout_title);

    let connected_outputs = state::niri_snapshot()
        .ok()
        .map(|snapshot| {
            snapshot
                .outputs
                .into_iter()
                .filter(|output| output.width.is_some())
                .map(|output| output.name)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let current_output = draft.borrow().output.clone();
    let (output, output_options) =
        output_dropdown(language, current_output.as_deref(), &connected_outputs);
    output.set_tooltip_text(Some(tr(language, "output")));
    {
        let draft = Rc::clone(&draft);
        output.connect_selected_notify(move |dropdown| {
            let selected = usize::try_from(dropdown.selected()).unwrap_or(0);
            draft.borrow_mut().output = output_options.get(selected).cloned().flatten();
        });
    }
    toolbar.append(&output);

    let workspace_entry = gtk::Entry::new();
    workspace_entry.set_width_chars(16);
    workspace_entry.set_text(&draft.borrow().workspace);
    workspace_entry.set_tooltip_text(Some(tr(language, "workspace_name")));
    {
        let draft = Rc::clone(&draft);
        let errors = Rc::clone(&validation_errors);
        workspace_entry.connect_changed(move |entry| {
            let value = entry.text().to_string();
            draft.borrow_mut().workspace = value.clone();
            let error = value.trim().is_empty().then(|| {
                if language == Language::Ru {
                    "Имя workspace не может быть пустым".to_owned()
                } else {
                    "Workspace name cannot be empty".to_owned()
                }
            });
            set_entry_validation(entry, &errors, "workspace", error);
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

        if !state::move_window_to_window(&mut draft_for_drop.borrow_mut(), &dragged, &target, after)
        {
            return;
        }

        show_editor_session(
            &state,
            &key_for_drop,
            Rc::clone(&draft_for_drop),
            Some(dragged),
        );
    });

    let weak = Rc::downgrade(state);
    let key_for_new_column = key.to_owned();
    let draft_for_new_column = Rc::clone(&draft);
    let new_column_callback: SimpleDropCallback = Rc::new(move |dragged| {
        let Some(state) = weak.upgrade() else {
            return;
        };

        if !state::move_window_to_new_column(&mut draft_for_new_column.borrow_mut(), &dragged) {
            return;
        }

        show_editor_session(
            &state,
            &key_for_new_column,
            Rc::clone(&draft_for_new_column),
            Some(dragged),
        );
    });

    let weak = Rc::downgrade(state);
    let key_for_float = key.to_owned();
    let draft_for_float = Rc::clone(&draft);
    let float_callback: SimpleDropCallback = Rc::new(move |dragged| {
        let Some(state) = weak.upgrade() else {
            return;
        };

        if !state::set_window_floating(&mut draft_for_float.borrow_mut(), &dragged, true) {
            return;
        }

        show_editor_session(
            &state,
            &key_for_float,
            Rc::clone(&draft_for_float),
            Some(dragged),
        );
    });

    layout_panel.append(&build_layout_canvas(
        &draft.borrow(),
        language,
        selected.as_deref(),
        Some(callback),
        Some(Rc::clone(&drop_callback)),
        Some(new_column_callback),
        Some(float_callback),
    ));
    left.append(&layout_panel);

    let details = gtk::Box::new(gtk::Orientation::Vertical, 11);
    details.add_css_class("card");
    details.add_css_class("editor-details-card");
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
            key,
            &details,
            Rc::clone(&draft),
            &selected_name,
            language,
            Rc::clone(&validation_errors),
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
    windows.add_css_class("windows-panel");
    windows.set_size_request(260, -1);

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
    add_window.add_css_class("quiet-action");
    add_window.set_tooltip_text(Some(if language == Language::Ru {
        "Добавить окно · Ctrl+N"
    } else {
        "Add window · Ctrl+N"
    }));
    let weak = Rc::downgrade(state);
    let key_for_add = key.to_owned();
    let draft_for_add = Rc::clone(&draft);
    add_window.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            show_add_window_dialog(&state, &key_for_add, Rc::clone(&draft_for_add));
        }
    });
    windows_header.append(&add_window);
    windows.append(&windows_header);

    for spec in &draft.borrow().windows {
        let button = gtk::Button::new();
        button.add_css_class("window-list-row");
        if selected_name.as_deref() == Some(spec.name.as_str()) {
            button.add_css_class("window-list-selected");
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

        button.set_child(Some(&row));

        let source = gtk::DragSource::new();
        source.set_actions(gtk::gdk::DragAction::MOVE);
        let drag_name = spec.name.clone();
        source.connect_prepare(move |_, _, _| {
            Some(gtk::gdk::ContentProvider::for_value(&drag_name.to_value()))
        });
        button.add_controller(source);

        let target = gtk::DropTarget::new(glib::Type::STRING, gtk::gdk::DragAction::MOVE);
        {
            let callback = Rc::clone(&drop_callback);
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
    let validation_for_save = Rc::clone(&validation_errors);
    let selected_for_save = selected_name.clone();
    save.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            save_editor_draft(
                &state,
                &key_for_save,
                Rc::clone(&draft_for_save),
                &validation_for_save,
                selected_for_save.clone(),
            );
        }
    });

    state.window.remove_action("save-editor");
    let save_action = gtk::gio::SimpleAction::new("save-editor", None);
    {
        let weak = Rc::downgrade(state);
        let key_owned = key.to_owned();
        let draft_owned = Rc::clone(&draft);
        let validation_owned = Rc::clone(&validation_errors);
        let selected_owned = selected_name.clone();
        save_action.connect_activate(move |_, _| {
            if let Some(state) = weak.upgrade() {
                save_editor_draft(
                    &state,
                    &key_owned,
                    Rc::clone(&draft_owned),
                    &validation_owned,
                    selected_owned.clone(),
                );
            }
        });
    }
    state.window.add_action(&save_action);

    state.window.remove_action("leave-editor");
    let leave_action = gtk::gio::SimpleAction::new("leave-editor", None);
    {
        let weak = Rc::downgrade(state);
        let key_owned = key.to_owned();
        let draft_owned = Rc::clone(&draft);
        let validation_owned = Rc::clone(&validation_errors);
        leave_action.connect_activate(move |_, _| {
            if let Some(state) = weak.upgrade() {
                request_leave_editor(
                    &state,
                    &key_owned,
                    Rc::clone(&draft_owned),
                    Rc::clone(&validation_owned),
                );
            }
        });
    }
    state.window.add_action(&leave_action);

    state.window.remove_action("add-window");
    let add_window_action = gtk::gio::SimpleAction::new("add-window", None);
    {
        let weak = Rc::downgrade(state);
        let key_owned = key.to_owned();
        let draft_owned = Rc::clone(&draft);
        add_window_action.connect_activate(move |_, _| {
            if let Some(state) = weak.upgrade() {
                show_add_window_dialog(&state, &key_owned, Rc::clone(&draft_owned));
            }
        });
    }
    state.window.add_action(&add_window_action);
    if let Some(app) = state.window.application() {
        app.set_accels_for_action("win.add-window", &["<Primary>n"]);
        app.set_accels_for_action("win.save-editor", &["<Primary>s"]);
        app.set_accels_for_action("win.leave-editor", &["<Alt>Left"]);
    }

    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&page)
        .build()
}

fn same_window_match(left: &WindowSpec, right: &WindowSpec) -> bool {
    left.match_spec.app_id == right.match_spec.app_id
        && left.match_spec.title == right.match_spec.title
        && left.match_spec.process == right.match_spec.process
}

fn append_editor_window(draft: &Rc<RefCell<Recipe>>, mut candidate: WindowSpec) -> String {
    let mut recipe = draft.borrow_mut();
    let base = if candidate.name.trim().is_empty() {
        "window".to_owned()
    } else {
        candidate.name.clone()
    };
    let mut name = base.clone();
    let mut suffix = 2usize;
    while recipe.windows.iter().any(|window| window.name == name) {
        name = format!("{base}-{suffix}");
        suffix += 1;
    }
    candidate.name = name.clone();

    if !candidate.layout.floating {
        candidate.layout.column = recipe
            .windows
            .iter()
            .filter(|window| !window.layout.floating)
            .map(|window| window.layout.column)
            .max()
            .unwrap_or(0)
            + 1;
        candidate.layout.column_width = None;
        candidate.layout.window_height = None;
        candidate.layout.display = None;
    }

    recipe.windows.push(candidate);
    state::normalize_columns(&mut recipe);
    name
}

fn installed_app_window_spec(app: &state::InstalledApp) -> WindowSpec {
    let mut identities = Vec::<String>::new();
    for value in [
        app.startup_wm_class.as_deref(),
        Some(app.desktop_id.as_str()),
        app.flatpak_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if !identities.iter().any(|existing| existing == value) {
            identities.push(value.to_owned());
        }
    }

    let app_id = match identities.as_slice() {
        [] => None,
        [one] => Some(format!("^{}$", regex::escape(one))),
        many => Some(format!(
            "^(?:{})$",
            many.iter()
                .map(|value| regex::escape(value))
                .collect::<Vec<_>>()
                .join("|")
        )),
    };

    WindowSpec {
        name: state::unique_slug(&app.name),
        command: app.command.clone(),
        match_spec: MatchSpec {
            app_id,
            title: None,
            process: None,
            pid: None,
            window_id: None,
        },
        reuse: ReusePolicy::Unique,
        layout: PlacementSpec::default(),
    }
}

fn show_add_window_dialog(state: &Rc<UiState>, key: &str, draft: Rc<RefCell<Recipe>>) {
    let language = state.language.get();
    let snapshot = match state::niri_snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            toast_message(state, &error.to_string());
            return;
        }
    };
    let captured = match state::capture_workspace(&snapshot, None) {
        Ok(captured) => captured,
        Err(error) => {
            toast_message(state, &error.to_string());
            return;
        }
    };

    let existing = draft.borrow().windows.clone();
    let candidates = captured
        .recipe
        .windows
        .into_iter()
        .zip(captured.details)
        .filter(|(candidate, _)| {
            !existing
                .iter()
                .any(|window| same_window_match(window, candidate))
        })
        .collect::<Vec<_>>();

    let installed_apps = state::installed_applications()
        .into_iter()
        .filter(|app| !app.command.is_empty())
        .filter(|app| {
            !app.desktop_id
                .eq_ignore_ascii_case("dev.t1ktak.NiriWorkbench")
        })
        .filter(|app| {
            !app.command
                .iter()
                .any(|part| part.contains("niri-workbench"))
        })
        .filter(|app| {
            let candidate = installed_app_window_spec(app);
            !existing
                .iter()
                .any(|window| same_window_match(window, &candidate))
        })
        .collect::<Vec<_>>();

    if candidates.is_empty() && installed_apps.is_empty() {
        toast_message(
            state,
            if language == Language::Ru {
                "Не найдено новых окон или установленных приложений"
            } else {
                "No new windows or installed applications were found"
            },
        );
        return;
    }

    let dialog = gtk::Window::builder()
        .title(if language == Language::Ru {
            "Добавить окно"
        } else {
            "Add window"
        })
        .transient_for(&state.window)
        .modal(true)
        .default_width(560)
        .default_height(620)
        .build();

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(true);
    header.set_decoration_layout(Some(":close"));
    let title = gtk::Label::new(Some(if language == Language::Ru {
        "Добавить окно"
    } else {
        "Add window"
    }));
    title.add_css_class("heading");
    header.set_title_widget(Some(&title));
    outer.append(&header);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.add_css_class("content");

    let hint = gtk::Label::new(Some(if language == Language::Ru {
        "Добавь уже открытое окно или выбери приложение, которое реально установлено в системе."
    } else {
        "Add an open window or choose an application that is actually installed on this system."
    }));
    hint.set_xalign(0.0);
    hint.set_wrap(true);
    hint.add_css_class("subtitle");
    root.append(&hint);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(if language == Language::Ru {
        "Найти установленное приложение…"
    } else {
        "Find an installed application…"
    }));
    search.add_css_class("search");
    root.append(&search);

    let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let mut first_candidate = None::<gtk::Button>;

    if !candidates.is_empty() {
        let heading = gtk::Label::new(Some(if language == Language::Ru {
            "Открытые окна"
        } else {
            "Open windows"
        }));
        heading.set_xalign(0.0);
        heading.add_css_class("section-title");
        list.append(&heading);
    }
    for (candidate, detail) in candidates {
        let row = gtk::Button::new();
        if first_candidate.is_none() {
            first_candidate = Some(row.clone());
        }
        row.add_css_class("window-picker-row");

        let inside = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        inside.append(&app_icon_tile(&candidate, 26, true));

        let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
        labels.set_hexpand(true);
        let name = gtk::Label::new(Some(&detail.title));
        name.set_xalign(0.0);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        labels.append(&name);
        let meta = gtk::Label::new(Some(&detail.app_id));
        meta.set_xalign(0.0);
        meta.add_css_class("subtitle");
        labels.append(&meta);
        inside.append(&labels);
        row.set_child(Some(&inside));

        let weak = Rc::downgrade(state);
        let key_owned = key.to_owned();
        let draft_owned = Rc::clone(&draft);
        let dialog_for_add = dialog.clone();
        row.connect_clicked(move |_| {
            let selected_name = append_editor_window(&draft_owned, candidate.clone());

            dialog_for_add.close();
            if let Some(state) = weak.upgrade() {
                show_editor_session(
                    &state,
                    &key_owned,
                    Rc::clone(&draft_owned),
                    Some(selected_name),
                );
            }
        });

        list.append(&row);
    }

    let mut installed_rows = Vec::<(String, gtk::Button)>::new();
    if !installed_apps.is_empty() {
        let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
        separator.set_margin_top(4);
        separator.set_margin_bottom(4);
        list.append(&separator);

        let heading = gtk::Label::new(Some(if language == Language::Ru {
            "Установленные приложения"
        } else {
            "Installed applications"
        }));
        heading.set_xalign(0.0);
        heading.add_css_class("section-title");
        list.append(&heading);
    }

    for app in installed_apps {
        let candidate = installed_app_window_spec(&app);
        let row = gtk::Button::new();
        if first_candidate.is_none() {
            first_candidate = Some(row.clone());
        }
        row.add_css_class("window-picker-row");
        row.set_tooltip_text(Some(&state::display_command(&app.command)));

        let inside = gtk::Box::new(gtk::Orientation::Horizontal, 10);

        let tile = gtk::Box::new(gtk::Orientation::Vertical, 0);
        tile.add_css_class("app-icon-small");
        tile.set_halign(gtk::Align::Center);
        tile.set_valign(gtk::Align::Center);
        let icon =
            gtk::Image::from_icon_name(app.icon.as_deref().unwrap_or("application-x-executable"));
        icon.set_pixel_size(26);
        tile.append(&icon);
        inside.append(&tile);

        let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
        labels.set_hexpand(true);

        let name = gtk::Label::new(Some(&app.name));
        name.set_xalign(0.0);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        labels.append(&name);

        let source = app
            .flatpak_id
            .as_ref()
            .map(|id| format!("Flatpak · {id}"))
            .unwrap_or_else(|| app.desktop_id.clone());
        let meta = gtk::Label::new(Some(&source));
        meta.set_xalign(0.0);
        meta.add_css_class("subtitle");
        labels.append(&meta);

        inside.append(&labels);
        row.set_child(Some(&inside));

        let haystack = format!(
            "{} {} {} {}",
            app.name,
            app.desktop_id,
            app.startup_wm_class.as_deref().unwrap_or_default(),
            app.flatpak_id.as_deref().unwrap_or_default()
        )
        .to_ascii_lowercase();
        installed_rows.push((haystack, row.clone()));

        let weak = Rc::downgrade(state);
        let key_owned = key.to_owned();
        let draft_owned = Rc::clone(&draft);
        let dialog_for_add = dialog.clone();
        row.connect_clicked(move |_| {
            let selected_name = append_editor_window(&draft_owned, candidate.clone());

            dialog_for_add.close();
            if let Some(state) = weak.upgrade() {
                show_editor_session(
                    &state,
                    &key_owned,
                    Rc::clone(&draft_owned),
                    Some(selected_name),
                );
            }
        });

        list.append(&row);
    }

    search.connect_search_changed(move |entry| {
        let query = entry.text().trim().to_ascii_lowercase();
        for (haystack, row) in &installed_rows {
            row.set_visible(query.is_empty() || haystack.contains(&query));
        }
    });

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();
    root.append(&scroller);

    outer.append(&root);
    dialog.set_child(Some(&outer));
    dialog.present();
    if let Some(first_candidate) = first_candidate {
        first_candidate.grab_focus();
    }
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

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(true);
    header.set_decoration_layout(Some(":close"));
    let title = gtk::Label::new(Some(if language == Language::Ru {
        "Предпросмотр макета"
    } else {
        "Layout preview"
    }));
    title.add_css_class("heading");
    header.set_title_widget(Some(&title));
    outer.append(&header);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 14);
    root.add_css_class("content");

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

    let canvas = build_layout_canvas(recipe, language, None, None, None, None, None);
    canvas.set_vexpand(true);
    root.append(&canvas);
    outer.append(&root);
    preview.set_child(Some(&outer));
    preview.present();
}

fn build_selected_window_controls(
    state: &Rc<UiState>,
    key: &str,
    container: &gtk::Box,
    draft: Rc<RefCell<Recipe>>,
    selected_name: &str,
    language: Language,
    validation_errors: Rc<RefCell<HashMap<String, String>>>,
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

    let remove = icon_button("user-trash-symbolic", "remove-window-button");
    remove.add_css_class("quiet-action");
    remove.add_css_class("destructive-action");
    remove.set_tooltip_text(Some(if language == Language::Ru {
        "Удалить окно из окружения"
    } else {
        "Remove window from workbench"
    }));
    {
        let weak = Rc::downgrade(state);
        let key_owned = key.to_owned();
        let draft = Rc::clone(&draft);
        let selected_name = current.name.clone();
        remove.connect_clicked(move |_| {
            let next_selected = state::remove_window(&mut draft.borrow_mut(), &selected_name);

            if let Some(state) = weak.upgrade() {
                show_editor_session(&state, &key_owned, Rc::clone(&draft), next_selected);
            }
        });
    }
    heading.append(&remove);
    container.append(&heading);

    let placement = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    placement.add_css_class("segmented-control");
    let tiled = gtk::ToggleButton::with_label(if language == Language::Ru {
        "В раскладке"
    } else {
        "Tiled"
    });
    let floating = gtk::ToggleButton::with_label(if language == Language::Ru {
        "Плавающее"
    } else {
        "Floating"
    });
    tiled.add_css_class("segment-button");
    floating.add_css_class("segment-button");
    floating.set_group(Some(&tiled));
    if current.layout.floating {
        floating.set_active(true);
    } else {
        tiled.set_active(true);
    }
    placement.append(&tiled);
    placement.append(&floating);
    container.append(&labeled_control(
        if language == Language::Ru {
            "Размещение"
        } else {
            "Placement"
        },
        &placement,
    ));

    {
        let weak = Rc::downgrade(state);
        let key_owned = key.to_owned();
        let draft_owned = Rc::clone(&draft);
        let selected_name = current.name.clone();
        tiled.connect_toggled(move |button| {
            if !button.is_active() {
                return;
            }
            if !state::set_window_floating(&mut draft_owned.borrow_mut(), &selected_name, false) {
                return;
            }
            if let Some(state) = weak.upgrade() {
                show_editor_session(
                    &state,
                    &key_owned,
                    Rc::clone(&draft_owned),
                    Some(selected_name.clone()),
                );
            }
        });
    }

    {
        let weak = Rc::downgrade(state);
        let key_owned = key.to_owned();
        let draft_owned = Rc::clone(&draft);
        let selected_name = current.name.clone();
        floating.connect_toggled(move |button| {
            if !button.is_active() {
                return;
            }
            if !state::set_window_floating(&mut draft_owned.borrow_mut(), &selected_name, true) {
                return;
            }
            if let Some(state) = weak.upgrade() {
                show_editor_session(
                    &state,
                    &key_owned,
                    Rc::clone(&draft_owned),
                    Some(selected_name.clone()),
                );
            }
        });
    }

    if !current.layout.floating {
        let display_mode = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        display_mode.add_css_class("segmented-control");
        let normal = gtk::ToggleButton::with_label(tr(language, "normal"));
        let tabbed = gtk::ToggleButton::with_label(tr(language, "tabbed"));
        normal.add_css_class("segment-button");
        tabbed.add_css_class("segment-button");
        tabbed.set_group(Some(&normal));
        if matches!(current.layout.display, Some(ColumnDisplay::Tabbed)) {
            tabbed.set_active(true);
        } else {
            normal.set_active(true);
        }
        display_mode.append(&normal);
        display_mode.append(&tabbed);
        container.append(&labeled_control(
            tr(language, "display_mode"),
            &display_mode,
        ));

        let column = current.layout.column;
        {
            let weak = Rc::downgrade(state);
            let key_owned = key.to_owned();
            let draft = Rc::clone(&draft);
            let selected_name = current.name.clone();
            normal.connect_toggled(move |button| {
                if !button.is_active() {
                    return;
                }
                state::set_column_display(
                    &mut draft.borrow_mut(),
                    column,
                    Some(ColumnDisplay::Normal),
                );
                if let Some(state) = weak.upgrade() {
                    show_editor_session(
                        &state,
                        &key_owned,
                        Rc::clone(&draft),
                        Some(selected_name.clone()),
                    );
                }
            });
        }
        {
            let weak = Rc::downgrade(state);
            let key_owned = key.to_owned();
            let draft = Rc::clone(&draft);
            let selected_name = current.name.clone();
            tabbed.connect_toggled(move |button| {
                if !button.is_active() {
                    return;
                }
                state::set_column_display(
                    &mut draft.borrow_mut(),
                    column,
                    Some(ColumnDisplay::Tabbed),
                );
                if let Some(state) = weak.upgrade() {
                    show_editor_session(
                        &state,
                        &key_owned,
                        Rc::clone(&draft),
                        Some(selected_name.clone()),
                    );
                }
            });
        }
    }

    let command = gtk::Entry::new();
    command.set_text(&state::display_command(&current.command));
    command.set_placeholder_text(Some(tr(language, "reuse_only")));
    container.append(&labeled_control(tr(language, "launch_command"), &command));
    {
        let draft = Rc::clone(&draft);
        let errors = Rc::clone(&validation_errors);
        command.connect_changed(
            move |entry| match state::parse_command(entry.text().as_str()) {
                Ok(command) => {
                    if let Some(window) = draft.borrow_mut().windows.get_mut(index) {
                        window.command = command;
                    }
                    set_entry_validation(entry, &errors, "command", None);
                }
                Err(error) => {
                    set_entry_validation(entry, &errors, "command", Some(error.to_string()));
                }
            },
        );
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

    if !current.layout.floating {
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
                let errors = Rc::clone(&validation_errors);
                width.connect_changed(move |entry| {
                    let text = entry.text();
                    if text.trim().is_empty() {
                        if let Some(window) = draft.borrow_mut().windows.get_mut(index) {
                            window.layout.column_width = None;
                        }
                        set_entry_validation(entry, &errors, "column-width", None);
                        return;
                    }

                    match Size::from_str(text.trim()) {
                        Ok(value) => {
                            if let Some(window) = draft.borrow_mut().windows.get_mut(index) {
                                window.layout.column_width = Some(value);
                            }
                            set_entry_validation(entry, &errors, "column-width", None);
                        }
                        Err(error) => {
                            set_entry_validation(
                                entry,
                                &errors,
                                "column-width",
                                Some(error.to_owned()),
                            );
                        }
                    }
                });
            }
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
        let errors = Rc::clone(&validation_errors);
        height.connect_changed(move |entry| {
            let text = entry.text();
            if text.trim().is_empty() {
                if let Some(window) = draft.borrow_mut().windows.get_mut(index) {
                    window.layout.window_height = None;
                }
                set_entry_validation(entry, &errors, "window-height", None);
                return;
            }

            match Size::from_str(text.trim()) {
                Ok(value) => {
                    if let Some(window) = draft.borrow_mut().windows.get_mut(index) {
                        window.layout.window_height = Some(value);
                    }
                    set_entry_validation(entry, &errors, "window-height", None);
                }
                Err(error) => {
                    set_entry_validation(entry, &errors, "window-height", Some(error.to_owned()));
                }
            }
        });
    }

    let app_id = gtk::Entry::new();
    app_id.set_text(current.match_spec.app_id.as_deref().unwrap_or(""));
    advanced_box.append(&labeled_control(tr(language, "match_app"), &app_id));
    {
        let draft = Rc::clone(&draft);
        let errors = Rc::clone(&validation_errors);
        app_id.connect_changed(move |entry| {
            let value = entry.text().trim().to_owned();
            if value.is_empty() {
                if let Some(window) = draft.borrow_mut().windows.get_mut(index) {
                    window.match_spec.app_id = None;
                }
                set_entry_validation(entry, &errors, "match-app-id", None);
                return;
            }

            match Regex::new(&value) {
                Ok(_) => {
                    if let Some(window) = draft.borrow_mut().windows.get_mut(index) {
                        window.match_spec.app_id = Some(value);
                    }
                    set_entry_validation(entry, &errors, "match-app-id", None);
                }
                Err(error) => {
                    set_entry_validation(entry, &errors, "match-app-id", Some(error.to_string()));
                }
            }
        });
    }

    let title_match = gtk::Entry::new();
    title_match.set_text(current.match_spec.title.as_deref().unwrap_or(""));
    advanced_box.append(&labeled_control(tr(language, "match_title"), &title_match));
    {
        let draft = Rc::clone(&draft);
        let errors = Rc::clone(&validation_errors);
        title_match.connect_changed(move |entry| {
            let value = entry.text().trim().to_owned();
            if value.is_empty() {
                if let Some(window) = draft.borrow_mut().windows.get_mut(index) {
                    window.match_spec.title = None;
                }
                set_entry_validation(entry, &errors, "match-title", None);
                return;
            }

            match Regex::new(&value) {
                Ok(_) => {
                    if let Some(window) = draft.borrow_mut().windows.get_mut(index) {
                        window.match_spec.title = Some(value);
                    }
                    set_entry_validation(entry, &errors, "match-title", None);
                }
                Err(error) => {
                    set_entry_validation(entry, &errors, "match-title", Some(error.to_string()));
                }
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
        button.add_css_class("flat");
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
    let config_path = settings::config_path();
    let (config, config_error) = match state::load_config_or_empty(&config_path) {
        Ok(config) => (config, None),
        Err(error) => (
            Config {
                workbench: Default::default(),
            },
            Some(error.to_string()),
        ),
    };
    let snapshot = state::niri_snapshot().ok();

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(tr(language, "quick_launcher"))
        .default_width(760)
        .default_height(570)
        .build();
    window.add_css_class("quick-window");

    let toast = adw::ToastOverlay::new();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 13);
    root.add_css_class("quick-root");

    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(true);
    header.set_show_end_title_buttons(false);
    header.set_decoration_layout(Some("close:"));

    let title = gtk::Label::new(Some(tr(language, "quick_launcher")));
    title.add_css_class("heading");
    header.set_title_widget(Some(&title));

    let esc = gtk::Label::new(Some("Esc"));
    esc.add_css_class("dim-label");
    esc.add_css_class("shortcut-hint");
    header.pack_end(&esc);
    root.append(&header);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(tr(language, "search")));
    search.add_css_class("search");
    root.append(&search);

    let list = gtk::Box::new(gtk::Orientation::Vertical, 9);
    let items: Rc<RefCell<Vec<QuickLauncherItem>>> = Rc::new(RefCell::new(Vec::new()));
    let selected = Rc::new(Cell::new(None::<usize>));
    let summary_label = gtk::Label::new(None);
    summary_label.set_xalign(0.0);
    summary_label.set_wrap(true);
    summary_label.add_css_class("subtitle");
    summary_label.set_visible(false);

    for (index, (key, recipe)) in config.workbench.iter().enumerate() {
        let status = state::recipe_status(recipe, snapshot.as_ref());
        let row = gtk::Button::new();
        row.add_css_class("quick-result");

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
        let launch_action = quick_launch_action(&status);
        let primary_action = match launch_action {
            Some("repair") => tr(language, "repair"),
            Some(_) => tr(language, "open"),
            None if language == Language::Ru => "Недоступно",
            None => "Unavailable",
        };
        let action = gtk::Label::new(Some(&format!("Enter · {primary_action}")));
        action.add_css_class("dim-label");
        action.add_css_class("quick-action-hint");
        action_box.append(&action);
        if launch_action.is_some() {
            let repair_hint =
                gtk::Label::new(Some(&format!("Ctrl+Enter · {}", tr(language, "repair"))));
            repair_hint.add_css_class("subtitle");
            action_box.append(&repair_hint);
        }
        inside.append(&action_box);

        row.set_child(Some(&inside));

        let key_owned = key.clone();
        let quick_window = window.clone();
        let quick_toast = toast.clone();
        let quick_config = config_path.clone();
        let row_action = launch_action;
        let items_for_click = Rc::clone(&items);
        let selected_for_click = Rc::clone(&selected);
        let summary_for_click = summary_label.clone();
        row.connect_clicked(move |_| {
            set_quick_selection(
                &items_for_click,
                &selected_for_click,
                &summary_for_click,
                Some(index),
            );
            if let Some(action) = row_action {
                run_quick_cli(
                    &quick_window,
                    &quick_toast,
                    action,
                    key_owned.clone(),
                    quick_config.clone(),
                    language,
                );
            } else {
                let message = if language == Language::Ru {
                    "Это окружение сейчас нельзя запустить. Открой Workbench для подробностей."
                } else {
                    "This workbench cannot be launched right now. Open Workbench for details."
                };
                quick_toast.add_toast(adw::Toast::new(message));
            }
        });

        items.borrow_mut().push(QuickLauncherItem {
            haystack: format!("{key} {} {}", recipe_display_name(recipe), recipe.workspace)
                .to_ascii_lowercase(),
            key: key.clone(),
            status: status.clone(),
            button: row.clone(),
            summary: quick_summary(language, recipe, &status),
        });
        list.append(&row);
    }

    let items_for_search = Rc::clone(&items);
    let selected_for_search = Rc::clone(&selected);
    let summary_for_search = summary_label.clone();
    search.connect_search_changed(move |entry| {
        let query = entry.text().trim().to_ascii_lowercase();
        for item in items_for_search.borrow().iter() {
            item.button
                .set_visible(query.is_empty() || item.haystack.contains(&query));
        }
        set_quick_selection(
            &items_for_search,
            &selected_for_search,
            &summary_for_search,
            None,
        );
    });

    let items_for_enter = Rc::clone(&items);
    let selected_for_enter = Rc::clone(&selected);
    search.connect_activate(move |_| {
        if let Some(index) = selected_for_enter.get() {
            if let Some(item) = items_for_enter.borrow().get(index) {
                item.button.emit_clicked();
            }
        }
    });
    root.append(&list);

    if let Some(error) = config_error {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
        card.add_css_class("card");
        card.add_css_class("quick-error");

        let title = gtk::Label::new(Some(if language == Language::Ru {
            "Не удалось прочитать config.toml"
        } else {
            "Could not read config.toml"
        }));
        title.set_xalign(0.0);
        title.add_css_class("title-lg");
        title.add_css_class("status-error-text");
        card.append(&title);

        let detail = gtk::Label::new(Some(&error));
        detail.set_xalign(0.0);
        detail.set_wrap(true);
        detail.add_css_class("subtitle");
        card.append(&detail);
        root.append(&card);
    } else if config.workbench.is_empty() {
        let empty = gtk::Label::new(Some(if language == Language::Ru {
            "Нет сохранённых окружений"
        } else {
            "No saved workbenches"
        }));
        empty.add_css_class("subtitle");
        empty.set_margin_top(24);
        empty.set_margin_bottom(24);
        root.append(&empty);
    }

    let summary_row = gtk::Box::new(gtk::Orientation::Horizontal, 9);
    summary_row.add_css_class("footer");
    let info = gtk::Image::from_icon_name("dialog-information-symbolic");
    info.set_pixel_size(17);
    summary_row.append(&info);
    summary_row.append(&summary_label);
    root.append(&summary_row);

    set_quick_selection(&items, &selected, &summary_label, None);

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
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let items_for_keys = Rc::clone(&items);
    let selected_for_keys = Rc::clone(&selected);
    let summary_for_keys = summary_label.clone();
    let quick_window = window.clone();
    let quick_toast = toast.clone();
    let quick_config = config_path.clone();
    controller.connect_key_pressed(move |_, key, _, modifiers| {
        if key == gtk::gdk::Key::Escape {
            quick_window.close();
            return glib::Propagation::Stop;
        }
        if key == gtk::gdk::Key::Down {
            move_quick_selection(&items_for_keys, &selected_for_keys, &summary_for_keys, 1);
            return glib::Propagation::Stop;
        }
        if key == gtk::gdk::Key::Up {
            move_quick_selection(&items_for_keys, &selected_for_keys, &summary_for_keys, -1);
            return glib::Propagation::Stop;
        }
        if key == gtk::gdk::Key::Return && modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
        {
            if let Some(index) = selected_for_keys.get() {
                if let Some(item) = items_for_keys.borrow().get(index).cloned() {
                    if quick_launch_action(&item.status).is_some() {
                        run_quick_cli(
                            &quick_window,
                            &quick_toast,
                            "repair",
                            item.key,
                            quick_config.clone(),
                            language,
                        );
                    } else {
                        let message = if language == Language::Ru {
                            "Repair недоступен для этого окружения."
                        } else {
                            "Repair is unavailable for this workbench."
                        };
                        quick_toast.add_toast(adw::Toast::new(message));
                    }
                }
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    window.add_controller(controller);

    toast.set_child(Some(&root));
    window.set_content(Some(&toast));
    window.present();
    state::shape_own_window(780, 570);
    search.grab_focus();
}
