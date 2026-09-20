use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub workbench: BTreeMap<String, Recipe>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Recipe {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub workspace: String,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub output_fallback: OutputFallback,
    #[serde(default)]
    pub focus: Option<String>,
    #[serde(default = "default_spawn_timeout_ms")]
    pub spawn_timeout_ms: u64,
    #[serde(default)]
    pub windows: Vec<WindowSpec>,
}

const fn default_spawn_timeout_ms() -> u64 {
    10_000
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct WindowSpec {
    pub name: String,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(rename = "match", default)]
    pub match_spec: MatchSpec,
    #[serde(default)]
    pub reuse: ReusePolicy,
    #[serde(default)]
    pub layout: PlacementSpec,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct MatchSpec {
    pub window_id: Option<u64>,
    pub app_id: Option<String>,
    pub title: Option<String>,
    pub process: Option<String>,
    pub cwd: Option<String>,
    pub pid: Option<i32>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReusePolicy {
    #[default]
    Unique,
    Never,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct PlacementSpec {
    #[serde(default = "default_column")]
    pub column: usize,
    #[serde(default)]
    pub column_width: Option<Size>,
    #[serde(default)]
    pub window_height: Option<Size>,
    #[serde(default)]
    pub display: Option<ColumnDisplay>,
    #[serde(default)]
    pub floating: bool,
    #[serde(default)]
    pub output: Option<String>,
}

impl Default for PlacementSpec {
    fn default() -> Self {
        Self {
            column: 1,
            column_width: None,
            window_height: None,
            display: None,
            floating: false,
            output: None,
        }
    }
}

const fn default_column() -> usize {
    1
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ColumnDisplay {
    Normal,
    Tabbed,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OutputFallback {
    #[default]
    Focused,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Size {
    Percent(f64),
    Pixels(i32),
}

impl FromStr for Size {
    type Err = &'static str;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let raw = raw.trim();
        if let Some(percent) = raw.strip_suffix('%') {
            let value: f64 = percent.trim().parse().map_err(|_| "invalid percentage")?;
            if !value.is_finite() || value <= 0.0 || value > 100.0 {
                return Err("percentage must be > 0 and <= 100");
            }
            return Ok(Self::Percent(value / 100.0));
        }
        let raw = raw.strip_suffix("px").unwrap_or(raw).trim();
        let value: i32 = raw.parse().map_err(|_| "invalid pixel value")?;
        if value <= 0 {
            return Err("pixel value must be positive");
        }
        Ok(Self::Pixels(value))
    }
}

impl<'de> Deserialize<'de> for Size {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::from_str(&raw).map_err(serde::de::Error::custom)
    }
}

impl Serialize for Size {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Size::Percent(v) => write!(f, "{}%", (v * 100.0).round()),
            Size::Pixels(v) => write!(f, "{v}px"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeWindow {
    pub id: u64,
    pub title: Option<String>,
    pub app_id: Option<String>,
    pub pid: Option<i32>,
    pub process_exe: Option<String>,
    pub cwd: Option<String>,
    pub workspace_id: Option<u64>,
    pub is_focused: bool,
    pub is_floating: bool,
    pub column: Option<usize>,
    pub tile_index: Option<usize>,
    pub tile_width: f64,
    pub tile_height: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeWorkspace {
    pub id: u64,
    pub index: u8,
    pub name: Option<String>,
    pub output: Option<String>,
    pub is_active: bool,
    pub is_focused: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OutputInfo {
    pub name: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ObservedState {
    pub windows: Vec<RuntimeWindow>,
    pub workspaces: Vec<RuntimeWorkspace>,
    pub outputs: Vec<OutputInfo>,
}

impl ObservedState {
    pub fn focused_window_id(&self) -> Option<u64> {
        self.windows.iter().find(|w| w.is_focused).map(|w| w.id)
    }

    pub fn focused_workspace(&self) -> Option<&RuntimeWorkspace> {
        self.workspaces.iter().find(|w| w.is_focused)
    }

    pub fn workspace(&self, id: u64) -> Option<&RuntimeWorkspace> {
        self.workspaces.iter().find(|w| w.id == id)
    }

    pub fn output(&self, name: &str) -> Option<&OutputInfo> {
        self.outputs.iter().find(|o| o.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::Size;
    use std::str::FromStr;

    #[test]
    fn parses_sizes() {
        assert_eq!(Size::from_str("55%").unwrap(), Size::Percent(0.55));
        assert_eq!(Size::from_str("900").unwrap(), Size::Pixels(900));
        assert_eq!(Size::from_str("900px").unwrap(), Size::Pixels(900));
    }

    #[test]
    fn rejects_bad_sizes() {
        assert!(Size::from_str("banana").is_err());
        assert!(Size::from_str("0%").is_err());
        assert!(Size::from_str("101%").is_err());
        assert!(Size::from_str("-2px").is_err());
    }
}
