use std::collections::HashSet;
use std::fs;
use std::path::Path;

use regex::Regex;
use thiserror::Error;

use crate::Recipe;
use crate::model::Config;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not read config {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("could not parse config {path} at {field}: {source}")]
    Parse {
        path: String,
        field: String,
        #[source]
        source: Box<toml::de::Error>,
    },
    #[error("{path}: {message}")]
    Validation { path: String, message: String },
}

pub fn load_config(path: &Path) -> Result<Config, ConfigError> {
    let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.display().to_string(),
        source,
    })?;
    parse_config(&text, &path.display().to_string())
}

pub fn parse_config(text: &str, source_name: &str) -> Result<Config, ConfigError> {
    let deserializer =
        toml::de::Deserializer::parse(text).map_err(|source| ConfigError::Parse {
            path: source_name.to_owned(),
            field: "<syntax>".to_owned(),
            source: Box::new(source),
        })?;
    let config: Config = serde_path_to_error::deserialize(deserializer).map_err(|error| {
        let field = error.path().to_string();
        ConfigError::Parse {
            path: source_name.to_owned(),
            field: if field.is_empty() {
                "<root>".to_owned()
            } else {
                field
            },
            source: Box::new(error.into_inner()),
        }
    })?;
    validate_config(&config)?;
    Ok(config)
}

fn validate_config(config: &Config) -> Result<(), ConfigError> {
    if config.workbench.is_empty() {
        return Err(validation("workbench", "no workbenches are defined"));
    }

    for (name, recipe) in &config.workbench {
        validate_recipe(name, recipe)?;
    }
    Ok(())
}

fn validate_recipe(name: &str, recipe: &Recipe) -> Result<(), ConfigError> {
    let base = format!("workbench.{name}");
    if recipe.workspace.trim().is_empty() {
        return Err(validation(
            format!("{base}.workspace"),
            "workspace name cannot be empty",
        ));
    }
    if recipe.windows.is_empty() {
        return Err(validation(
            format!("{base}.windows"),
            "at least one window is required",
        ));
    }

    let mut names = HashSet::new();
    for (idx, window) in recipe.windows.iter().enumerate() {
        let path = format!("{base}.windows[{idx}]");
        if window.name.trim().is_empty() {
            return Err(validation(
                format!("{path}.name"),
                "logical name cannot be empty",
            ));
        }
        if !names.insert(window.name.as_str()) {
            return Err(validation(
                format!("{path}.name"),
                format!("duplicate logical window name {:?}", window.name),
            ));
        }
        if window.layout.column == 0 {
            return Err(validation(
                format!("{path}.layout.column"),
                "column indices start at 1",
            ));
        }
        if window.command.is_empty() && matches!(window.reuse, crate::ReusePolicy::Never) {
            return Err(validation(
                format!("{path}.command"),
                r#"reuse = "never" requires a spawn command"#,
            ));
        }
        if window.match_spec.window_id.is_none()
            && window.match_spec.app_id.is_none()
            && window.match_spec.title.is_none()
            && window.match_spec.process.is_none()
            && window.match_spec.pid.is_none()
        {
            return Err(validation(
                format!("{path}.match"),
                "at least one matcher is required",
            ));
        }
        for (field, value) in [
            ("app_id", window.match_spec.app_id.as_deref()),
            ("title", window.match_spec.title.as_deref()),
            ("process", window.match_spec.process.as_deref()),
        ] {
            if let Some(pattern) = value {
                Regex::new(pattern).map_err(|err| {
                    validation(
                        format!("{path}.match.{field}"),
                        format!("invalid regex {pattern:?}: {err}"),
                    )
                })?;
            }
        }
        if window.layout.floating
            && (window.layout.display.is_some() || window.layout.column_width.is_some())
        {
            return Err(validation(
                format!("{path}.layout"),
                "floating windows cannot declare column display or column_width",
            ));
        }
        if let (Some(recipe_output), Some(window_output)) =
            (recipe.output.as_deref(), window.layout.output.as_deref())
        {
            if recipe_output != window_output {
                return Err(validation(
                    format!("{path}.layout.output"),
                    "v0.1 requires per-window output to match the workbench output;                      a Niri workspace belongs to one output",
                ));
            }
        }
    }

    let mut columns: std::collections::BTreeMap<
        usize,
        (Option<crate::Size>, Option<crate::ColumnDisplay>),
    > = std::collections::BTreeMap::new();
    let mut window_outputs = HashSet::new();
    for (idx, window) in recipe.windows.iter().enumerate() {
        if let Some(output) = window.layout.output.as_deref() {
            window_outputs.insert(output);
        }
        if window.layout.floating {
            continue;
        }
        let entry = columns.entry(window.layout.column).or_insert((None, None));
        if let Some(width) = window.layout.column_width {
            if entry.0.is_some_and(|existing| existing != width) {
                return Err(validation(
                    format!("{base}.windows[{idx}].layout.column_width"),
                    format!(
                        "conflicts with another width declared for column {}",
                        window.layout.column
                    ),
                ));
            }
            entry.0 = Some(width);
        }
        if let Some(display) = window.layout.display {
            if entry.1.is_some_and(|existing| existing != display) {
                return Err(validation(
                    format!("{base}.windows[{idx}].layout.display"),
                    format!(
                        "conflicts with another display mode declared for column {}",
                        window.layout.column
                    ),
                ));
            }
            entry.1 = Some(display);
        }
    }

    if window_outputs.len() > 1 {
        return Err(validation(
            format!("{base}.windows"),
            "windows in one workbench cannot target multiple outputs in v0.1 because a Niri workspace belongs to one output",
        ));
    }

    let expected_columns: Vec<usize> = (1..=columns.len()).collect();
    let actual_columns: Vec<usize> = columns.keys().copied().collect();
    if !columns.is_empty() && actual_columns != expected_columns {
        return Err(validation(
            format!("{base}.windows"),
            format!(
                "tiling column indices must be contiguous starting at 1; found {actual_columns:?}"
            ),
        ));
    }

    if let Some(focus) = &recipe.focus {
        if !names.contains(focus.as_str()) {
            return Err(validation(
                format!("{base}.focus"),
                format!("unknown logical window {focus:?}"),
            ));
        }
    }
    Ok(())
}

fn validation(path: impl Into<String>, message: impl Into<String>) -> ConfigError {
    ConfigError::Validation {
        path: path.into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_config;

    #[test]
    fn parses_minimal_recipe() {
        let config = parse_config(
            r#"
            [workbench.dev]
            workspace = "Dev"

            [[workbench.dev.windows]]
            name = "editor"
            command = ["code", "."]

            [workbench.dev.windows.match]
            app_id = "^code$"

            [workbench.dev.windows.layout]
            column = 1
            column_width = "55%"
            "#,
            "test",
        )
        .unwrap();

        assert_eq!(config.workbench["dev"].windows.len(), 1);
    }

    #[test]
    fn rejects_duplicate_window_names() {
        let err = parse_config(
            r#"
            [workbench.dev]
            workspace = "Dev"

            [[workbench.dev.windows]]
            name = "term"
            command = ["foot"]
            [workbench.dev.windows.match]
            app_id = "^foot$"

            [[workbench.dev.windows]]
            name = "term"
            command = ["foot"]
            [workbench.dev.windows.match]
            app_id = "^foot$"
            "#,
            "test",
        )
        .unwrap_err();

        assert!(err.to_string().contains("duplicate logical window name"));
    }

    #[test]
    fn rejects_invalid_regex() {
        let err = parse_config(
            r#"
            [workbench.dev]
            workspace = "Dev"

            [[workbench.dev.windows]]
            name = "term"
            command = ["foot"]
            [workbench.dev.windows.match]
            app_id = "["
            "#,
            "test",
        )
        .unwrap_err();

        assert!(err.to_string().contains("invalid regex"));
    }

    #[test]
    fn size_error_reports_field_path() {
        let err = parse_config(
            r#"
            [workbench.dev]
            workspace = "Dev"

            [[workbench.dev.windows]]
            name = "term"
            command = ["foot"]
            [workbench.dev.windows.match]
            app_id = "^foot$"
            [workbench.dev.windows.layout]
            column = 1
            column_width = "banana"
            "#,
            "test",
        )
        .unwrap_err();

        let text = err.to_string();
        assert!(
            text.contains("workbench.dev.windows[0].layout.column_width"),
            "{text}"
        );
        assert!(text.contains("invalid pixel value"), "{text}");
    }

    #[test]
    fn conflicting_column_display_is_rejected() {
        let err = parse_config(
            r#"
            [workbench.dev]
            workspace = "Dev"

            [[workbench.dev.windows]]
            name = "a"
            command = ["foot"]
            [workbench.dev.windows.match]
            app_id = "^a$"
            [workbench.dev.windows.layout]
            column = 1
            display = "tabbed"

            [[workbench.dev.windows]]
            name = "b"
            command = ["foot"]
            [workbench.dev.windows.match]
            app_id = "^b$"
            [workbench.dev.windows.layout]
            column = 1
            display = "normal"
            "#,
            "test",
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("conflicts with another display mode")
        );
    }
}
