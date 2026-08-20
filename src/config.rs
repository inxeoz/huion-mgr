use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Tablet device name pattern for matching in /proc/bus/input/devices
    pub tablet_name: String,
    /// Express key mappings
    pub express_keys: Vec<KeyBinding>,
    /// Pen settings
    pub pen: PenConfig,
    /// Hyprland-specific settings
    pub hyprland: Option<HyprlandConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PenConfig {
    /// Pressure curve: list of [input, output] control points (0.0-1.0)
    pub pressure_curve: Vec<[f32; 2]>,
    /// Map to specific output (monitor name or "all")
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HyprlandConfig {
    /// Monitor to map tablet to (e.g. "DP-1", "eDP-1")
    pub monitor: Option<String>,
    /// Custom tablet region [x, y, width, height] in mm
    pub region: Option<[f32; 4]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyBinding {
    /// Physical H951P key name, legacy evdev name, or numeric code.
    pub key: String,
    /// Human-readable name.
    pub name: String,
    /// Optional mode layer. None is the fallback for every mode.
    #[serde(default)]
    pub mode: Option<String>,
    /// Action to perform.
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum Action {
    /// Simulate keyboard shortcut via wtype (e.g. "ctrl+z")
    #[serde(rename = "combo")]
    KeyCombo(String),
    /// Simulate a single key press via wtype (e.g. "Return", "space")
    #[serde(rename = "key")]
    KeyPress(String),
    /// Run a shell command
    #[serde(rename = "command")]
    Command(String),
    /// Run a hyprctl dispatch command
    #[serde(rename = "hyprctl")]
    Hyprctl(String),
    /// Simulate mouse click (left, right, middle).
    #[serde(rename = "mouse")]
    MouseClick(String),
    /// Simulate a mouse-wheel scroll (up or down).
    #[serde(rename = "scroll")]
    MouseScroll(String),
    /// Disabled
    #[serde(rename = "none")]
    None,
}

impl Action {
    /// Parse the CLI form `kind:value`, for example `combo:ctrl+z`.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let spec = spec.trim();
        if spec == "none" {
            return Ok(Self::None);
        }

        let (kind, value) = spec
            .split_once(':')
            .ok_or_else(|| "action must use kind:value (for example combo:ctrl+z)".to_string())?;
        let value = value.trim();
        if value.is_empty() {
            return Err(format!("action '{kind}' needs a value"));
        }

        match kind.trim().to_ascii_lowercase().as_str() {
            "combo" | "c" => Ok(Self::KeyCombo(value.to_string())),
            "key" | "k" => Ok(Self::KeyPress(value.to_string())),
            "cmd" | "command" => Ok(Self::Command(value.to_string())),
            "hyprctl" | "h" => Ok(Self::Hyprctl(value.to_string())),
            "mouse" | "m" => Ok(Self::MouseClick(value.to_string())),
            "scroll" | "wheel" | "s" => Ok(Self::MouseScroll(value.to_string())),
            "none" => Err("none does not accept a value; use 'none'".to_string()),
            _ => Err(format!(
                "unknown action type '{kind}'; use combo, key, command, hyprctl, mouse, scroll, or none"
            )),
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            tablet_name: "Huion Tablet".to_string(),
            express_keys: default_keybindings(),
            pen: PenConfig {
                pressure_curve: vec![
                    [0.0, 0.0],
                    [0.25, 0.15],
                    [0.5, 0.5],
                    [0.75, 0.85],
                    [1.0, 1.0],
                ],
                output: "all".to_string(),
            },
            hyprland: Some(HyprlandConfig {
                monitor: None,
                region: None,
            }),
        }
    }
}

fn default_keybindings() -> Vec<KeyBinding> {
    [
        ("mode1", "Mode 1"),
        ("mode2", "Mode 2"),
        ("mode3", "Mode 3"),
        ("top-button1", "Top Button 1"),
        ("top-button2", "Top Button 2"),
        ("top-button3", "Top Button 3"),
        ("top-button4", "Top Button 4"),
        ("scroll-up", "Scroll Up"),
        ("scroll-down", "Scroll Down"),
        ("bottom-button1", "Bottom Button 1"),
        ("bottom-button2", "Bottom Button 2"),
        ("bottom-button3", "Bottom Button 3"),
        ("bottom-button4", "Bottom Button 4"),
    ]
    .into_iter()
    .map(|(key, name)| KeyBinding {
        key: key.into(),
        name: name.into(),
        mode: None,
        action: Action::None,
    })
    .collect()
}

impl AppConfig {
    pub fn config_dir() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("huion-mgr")
    }

    pub fn config_path() -> PathBuf {
        Self::config_dir().join("config.toml")
    }

    /// Path requested by `config generate`, independent of XDG_CONFIG_HOME.
    pub fn home_config_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("~"))
            .join(".config")
            .join("huion-mgr")
            .join("config.toml")
    }

    /// Load a config file, returning defaults when the file does not exist.
    pub fn load_from(path: &std::path::Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        toml::from_str(&content).map_err(|e| format!("failed to parse {}: {e}", path.display()))
    }

    pub fn save_to(&self, path: &std::path::Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
        }
        let content =
            toml::to_string_pretty(self).map_err(|e| format!("failed to serialize config: {e}"))?;
        std::fs::write(path, content)
            .map_err(|e| format!("failed to write {}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cli_actions() {
        assert_eq!(
            Action::parse("combo:ctrl+z"),
            Ok(Action::KeyCombo("ctrl+z".into()))
        );
        assert_eq!(
            Action::parse("key:Return"),
            Ok(Action::KeyPress("Return".into()))
        );
        assert_eq!(
            Action::parse("cmd:echo hello"),
            Ok(Action::Command("echo hello".into()))
        );
        assert_eq!(
            Action::parse("scroll:up"),
            Ok(Action::MouseScroll("up".into()))
        );
        assert_eq!(Action::parse("none"), Ok(Action::None));
    }

    #[test]
    fn rejects_invalid_cli_actions() {
        assert!(Action::parse("combo").is_err());
        assert!(Action::parse("unknown:value").is_err());
        assert!(Action::parse("none:value").is_err());
    }

    #[test]
    fn missing_fields_use_defaults() {
        let config: AppConfig = toml::from_str("profiles = []").unwrap();
        assert_eq!(config.tablet_name, "Huion Tablet");
        assert_eq!(config.express_keys.len(), 13);
    }

    #[test]
    fn config_saves_and_loads_from_a_custom_path() {
        let path =
            std::env::temp_dir().join(format!("huion-mgr-config-test-{}.toml", std::process::id()));
        let config = AppConfig::default();
        config.save_to(&path).unwrap();
        let loaded = AppConfig::load_from(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(loaded, config);
    }

    #[test]
    fn config_round_trips_readable_actions() {
        let config = AppConfig::default();
        let text = toml::to_string_pretty(&config).unwrap();
        assert!(text.contains("type = \"none\""));
        let decoded: AppConfig = toml::from_str(&text).unwrap();
        assert_eq!(decoded, config);
    }
}
