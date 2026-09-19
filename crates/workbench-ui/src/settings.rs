use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::i18n::Language;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSettings {
    #[serde(default = "default_language")]
    language: String,
}

fn default_language() -> String {
    env::var("LANG").unwrap_or_else(|_| "en".to_owned())
}

pub fn config_dir() -> PathBuf {
    if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(path).join("niri-workbench")
    } else {
        PathBuf::from(env::var_os("HOME").unwrap_or_else(|| ".".into()))
            .join(".config/niri-workbench")
    }
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn settings_path() -> PathBuf {
    config_dir().join("ui.toml")
}

pub fn load_language(path: &Path) -> Language {
    let stored = fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str::<StoredSettings>(&text).ok())
        .map(|settings| settings.language)
        .unwrap_or_else(default_language);
    Language::from_code(&stored)
}

pub fn save_language(path: &Path, language: Language) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(&StoredSettings {
        language: language.code().to_owned(),
    })
    .unwrap_or_else(|_| format!("language = {:?}\n", language.code()));
    fs::write(path, text)
}
