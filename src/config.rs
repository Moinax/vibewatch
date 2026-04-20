use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: GeneralConfig,
    pub sounds: SoundConfig,
    pub agents: HashMap<String, AgentConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    pub compositor: String,
    pub socket_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SoundConfig {
    pub enabled: bool,
    pub approval_needed: String,
    pub error: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    pub window_class: String,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            compositor: "auto".to_string(),
            socket_path: None,
        }
    }
}

impl Default for SoundConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            approval_needed: "builtin:chime".to_string(),
            error: "builtin:alert".to_string(),
        }
    }
}

impl Config {
    /// Returns the path to the config file: `$XDG_CONFIG_HOME/vibewatch/config.toml`
    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("vibewatch")
            .join("config.toml")
    }

    /// Returns the IPC socket path.
    /// Uses `$XDG_RUNTIME_DIR/vibewatch.sock` if available,
    /// otherwise falls back to `/tmp/vibewatch-$USER.sock`.
    pub fn socket_path(&self) -> PathBuf {
        if let Some(ref path) = self.general.socket_path {
            return PathBuf::from(path);
        }

        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            PathBuf::from(runtime_dir).join("vibewatch.sock")
        } else {
            let user = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
            PathBuf::from(format!("/tmp/vibewatch-{}.sock", user))
        }
    }

    /// Load configuration from the default config path.
    /// Returns the default config if the file doesn't exist.
    pub fn load() -> anyhow::Result<Self> {
        let path = Self::config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(&path)?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.general.compositor, "auto");
        assert!(config.general.socket_path.is_none());
        assert!(config.sounds.enabled);
        assert_eq!(config.sounds.approval_needed, "builtin:chime");
        assert_eq!(config.sounds.error, "builtin:alert");
        assert!(config.agents.is_empty());
    }

    #[test]
    fn test_parse_full_config() {
        let toml_str = r#"
[general]
compositor = "hyprland"
socket_path = "/run/user/1000/vw.sock"

[sounds]
enabled = false
approval_needed = "/home/user/chime.wav"
error = "/home/user/alert.wav"

[agents.claude]
window_class = "cursor"

[agents.copilot]
window_class = "code"
"#;
        let config = toml::from_str::<Config>(toml_str).unwrap();
        assert_eq!(config.general.compositor, "hyprland");
        assert_eq!(
            config.general.socket_path.as_deref(),
            Some("/run/user/1000/vw.sock")
        );
        assert!(!config.sounds.enabled);
        assert_eq!(config.sounds.approval_needed, "/home/user/chime.wav");
        assert_eq!(config.sounds.error, "/home/user/alert.wav");
        assert_eq!(config.agents.len(), 2);
        assert_eq!(config.agents["claude"].window_class, "cursor");
        assert_eq!(config.agents["copilot"].window_class, "code");
    }

    #[test]
    fn test_parse_empty_config() {
        let config = toml::from_str::<Config>("").unwrap();
        assert_eq!(config.general.compositor, "auto");
        assert!(config.sounds.enabled);
        assert!(config.agents.is_empty());
    }

    #[test]
    fn test_parse_partial_config() {
        let toml_str = r#"
[sounds]
enabled = false
"#;
        let config = toml::from_str::<Config>(toml_str).unwrap();
        // sounds section partially overridden
        assert!(!config.sounds.enabled);
        assert_eq!(config.sounds.approval_needed, "builtin:chime");
        // general should be default
        assert_eq!(config.general.compositor, "auto");
        assert!(config.general.socket_path.is_none());
        assert!(config.agents.is_empty());
    }

    #[test]
    fn test_socket_path_uses_xdg() {
        std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
        let config = Config::default();
        let path = config.socket_path();
        assert_eq!(path, PathBuf::from("/run/user/1000/vibewatch.sock"));
        std::env::remove_var("XDG_RUNTIME_DIR");
    }
}
