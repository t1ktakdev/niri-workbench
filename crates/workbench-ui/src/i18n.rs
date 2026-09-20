#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    En,
    Ru,
}

impl Language {
    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Ru => "ru",
        }
    }

    pub fn from_code(value: &str) -> Self {
        if value.eq_ignore_ascii_case("ru") || value.to_ascii_lowercase().starts_with("ru_") {
            Self::Ru
        } else {
            Self::En
        }
    }
}

pub fn tr(language: Language, key: &str) -> &str {
    match (language, key) {
        (Language::Ru, "home") => "Главная",
        (Language::Ru, "capture") => "Снимок",
        (Language::Ru, "library") => "Библиотека",
        (Language::Ru, "settings") => "Настройки",
        (Language::Ru, "new") => "Сохранить текущее",
        (Language::Ru, "search") => "Поиск рабочих окружений…",
        (Language::Ru, "open") => "Открыть",
        (Language::Ru, "repair") => "Исправить",
        (Language::Ru, "edit") => "Изменить",
        (Language::Ru, "ready") => "Готово",
        (Language::Ru, "layout_changed") => "Макет изменён",
        (Language::Ru, "offline") => "Niri недоступен",
        (Language::Ru, "missing_one") => "1 приложение отсутствует",
        (Language::Ru, "missing_many") => "приложений отсутствуют",
        (Language::Ru, "ambiguous") => "Нужно уточнить совпадение",
        (Language::Ru, "what_it_does") => "Что делает Workbench",
        (Language::Ru, "what_it_does_body") => {
            "Сохраняет рабочие окружения, открывает их одной кнопкой и восстанавливает раскладку без лишних копий окон."
        }
        (Language::Ru, "open_hint") => {
            "Открыть — запустить недостающее и восстановить макет · Исправить — только вернуть существующие окна"
        }
        (Language::Ru, "empty_title") => "Пока нет рабочих окружений",
        (Language::Ru, "empty_body") => {
            "Расставь реальные окна как тебе удобно и нажми «Сохранить текущее». Workbench снимет открытые окна, приложения и раскладку."
        }
        (Language::Ru, "capture_title") => "Сохранить текущее окружение",
        (Language::Ru, "capture_intro") => {
            "Workbench снимет реальные окна этого workspace, их приложения, позиции и доступный контекст запуска. Проверь результат и сохрани."
        }
        (Language::Ru, "detected_windows") => "Найденные окна",
        (Language::Ru, "workbench_details") => "Параметры окружения",
        (Language::Ru, "name") => "Имя",
        (Language::Ru, "workspace_name") => "Workspace",
        (Language::Ru, "output") => "Монитор",
        (Language::Ru, "auto") => "Авто",
        (Language::Ru, "launch_command") => "Команда запуска",
        (Language::Ru, "reuse_only") => "Только переиспользование",
        (Language::Ru, "create_workbench") => "Сохранить снимок",
        (Language::Ru, "cancel") => "Отмена",
        (Language::Ru, "edit_workbench") => "Редактор окружения",
        (Language::Ru, "save") => "Сохранить",
        (Language::Ru, "preview") => "Предпросмотр",
        (Language::Ru, "layout") => "Макет",
        (Language::Ru, "windows") => "Окна",
        (Language::Ru, "selected_window") => "Выбранное окно",
        (Language::Ru, "logical_name") => "Имя окна",
        (Language::Ru, "column") => "Колонка",
        (Language::Ru, "column_width") => "Ширина колонки",
        (Language::Ru, "window_height") => "Высота окна",
        (Language::Ru, "display_mode") => "Режим колонки",
        (Language::Ru, "normal") => "Обычный",
        (Language::Ru, "tabbed") => "Вкладки",
        (Language::Ru, "not_set") => "Авто",
        (Language::Ru, "final_focus") => "Фокус после открытия",
        (Language::Ru, "match_app") => "App ID",
        (Language::Ru, "match_title") => "Заголовок окна",
        (Language::Ru, "advanced") => "Дополнительно",
        (Language::Ru, "language") => "Язык",
        (Language::Ru, "language_help") => {
            "Язык интерфейса сохраняется отдельно от рецептов Workbench."
        }
        (Language::Ru, "appearance") => "Интерфейс",
        (Language::Ru, "quick_launcher") => "Быстрый запуск",
        (Language::Ru, "quick_hint") => "Enter — открыть · Ctrl+Enter — исправить",
        (Language::Ru, "capture_error") => "Не удалось прочитать текущий workspace",
        (Language::Ru, "saved") => "Сохранено",
        (Language::Ru, "opening") => "Открываю окружение…",
        (Language::Ru, "repairing") => "Восстанавливаю макет…",
        (Language::Ru, "config_error") => "Ошибка конфигурации",
        (Language::Ru, "refresh") => "Обновить",
        (_, "home") => "Home",
        (_, "capture") => "Snapshot",
        (_, "library") => "Library",
        (_, "settings") => "Settings",
        (_, "new") => "Save current",
        (_, "search") => "Search workbenches…",
        (_, "open") => "Open",
        (_, "repair") => "Repair",
        (_, "edit") => "Edit",
        (_, "ready") => "Ready",
        (_, "layout_changed") => "Layout changed",
        (_, "offline") => "Niri unavailable",
        (_, "missing_one") => "1 application missing",
        (_, "missing_many") => "applications missing",
        (_, "ambiguous") => "Matching needs attention",
        (_, "what_it_does") => "What Workbench does",
        (_, "what_it_does_body") => {
            "Save project workspaces, reopen them in one click, and repair layout drift without opening duplicates."
        }
        (_, "open_hint") => {
            "Open = launch missing apps and restore layout · Repair = move existing windows only"
        }
        (_, "empty_title") => "No workbenches yet",
        (_, "empty_body") => {
            "Arrange the real windows you want, then choose Save current. Workbench snapshots the open apps and their layout."
        }
        (_, "capture_title") => "Save current workspace",
        (_, "capture_intro") => {
            "Workbench snapshots the real windows on this workspace, their installed apps, positions, and available launch context. Review and save."
        }
        (_, "detected_windows") => "Detected windows",
        (_, "workbench_details") => "Workbench details",
        (_, "name") => "Name",
        (_, "workspace_name") => "Workspace",
        (_, "output") => "Output",
        (_, "auto") => "Auto",
        (_, "launch_command") => "Launch command",
        (_, "reuse_only") => "Reuse only",
        (_, "create_workbench") => "Save snapshot",
        (_, "cancel") => "Cancel",
        (_, "edit_workbench") => "Edit workbench",
        (_, "save") => "Save",
        (_, "preview") => "Preview",
        (_, "layout") => "Layout",
        (_, "windows") => "Windows",
        (_, "selected_window") => "Selected window",
        (_, "logical_name") => "Window name",
        (_, "column") => "Column",
        (_, "column_width") => "Column width",
        (_, "window_height") => "Window height",
        (_, "display_mode") => "Column mode",
        (_, "normal") => "Normal",
        (_, "tabbed") => "Tabbed",
        (_, "not_set") => "Auto",
        (_, "final_focus") => "Focus after opening",
        (_, "match_app") => "App ID",
        (_, "match_title") => "Window title",
        (_, "advanced") => "Advanced",
        (_, "language") => "Language",
        (_, "language_help") => "Interface language is stored separately from Workbench recipes.",
        (_, "appearance") => "Interface",
        (_, "quick_launcher") => "Quick launcher",
        (_, "quick_hint") => "Enter = open · Ctrl+Enter = repair",
        (_, "capture_error") => "Could not inspect the current workspace",
        (_, "saved") => "Saved",
        (_, "opening") => "Opening workbench…",
        (_, "repairing") => "Repairing layout…",
        (_, "config_error") => "Configuration error",
        (_, "refresh") => "Refresh",
        _ => key,
    }
}
