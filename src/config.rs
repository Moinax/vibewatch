use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: GeneralConfig,
    pub sounds: SoundConfig,
    pub panel: PanelConfig,
    pub t3: T3Config,
    pub limits: LimitsConfig,
    pub agents: HashMap<String, AgentConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    pub compositor: String,
    pub socket_path: Option<String>,
    /// How long a session has to stay idle before the finish is announced, in
    /// milliseconds. A turn that ends only to be resumed moments later — the
    /// agent launched a background sub-agent, or is between two reasoning steps
    /// — is a real `Stop`, so without this it earns a real chime and a real
    /// panel pop, several times per task. Waiting it out announces once, when
    /// the agent is actually done. Held here rather than under `sounds` or
    /// `panel` because it gates both. `0` restores announcing on every `Stop`.
    pub idle_debounce_ms: u64,
    /// How long a turn held open by outstanding sub-agents may stay silent
    /// before it is announced anyway, in milliseconds.
    ///
    /// The hold otherwise has no end: a sub-agent that dies without sending its
    /// `Stop` — a terminal API error, a killed pane — leaves the turn waiting
    /// on a report that will never come, and the finish is never announced at
    /// all. A missing chime is the worse failure of the two, so the hold
    /// expires.
    ///
    /// Generous on purpose: a live sub-agent's own tool hooks land on the parent
    /// session, so anything actually running keeps that session out of `Idle`
    /// and cancels this wait — only a session that has gone completely still is
    /// released. `0` lets a held turn stay silent indefinitely.
    pub hold_ceiling_ms: u64,
    /// How long a permission request has to still be outstanding before it is
    /// announced, in milliseconds.
    ///
    /// The hook fires whenever a decision is *asked for*, not when it reaches
    /// the user — an allowlist entry, a mode that auto-accepts, or T3 Code
    /// answering on its own resolves it without anyone ever seeing a prompt,
    /// and announcing on arrival means a chime and a panel pop for a session
    /// that never stopped working. Measured on this machine: auto-resolved
    /// requests come back in 7-130ms, while one actually waiting on the user
    /// sits for minutes — so the two are trivially separable by waiting.
    /// `0` restores announcing on every request.
    pub approval_debounce_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SoundConfig {
    pub enabled: bool,
    /// Played when the agent asks for approval/permission (a question).
    pub approval_needed: String,
    /// Played when the agent finishes responding and goes idle.
    pub idle: String,
    /// Reserved for error alerts; not auto-triggered by the daemon.
    pub error: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PanelConfig {
    /// Slide the panel in/out with a drawer animation on toggle.
    /// When false, the panel snaps instantly (animation_ms is ignored).
    pub animate: bool,
    /// Drawer slide duration in milliseconds.
    pub animation_ms: u32,
    /// Auto-hide the panel once nothing needs attention and the pointer is
    /// not over it.
    ///
    /// Only a session waiting for approval counts as needing attention, and it
    /// holds the panel open indefinitely — the timer is reset, not merely
    /// deferred — because it is the one state that cannot proceed without you.
    /// A finished turn does not hold the drawer: closing loses nothing, since
    /// `Session::just_finished` is not time-based, so the row stays lit until
    /// the row's `Seen` bar or a click on the card acknowledges it, or that
    /// agent picks the work back up.
    pub auto_close: bool,
    /// How long the pointer has to stay away before the panel closes, in
    /// milliseconds. Only starts counting once nothing needs attention in the
    /// sense above.
    pub auto_close_ms: u64,
    /// Pop the panel open when an agent finishes its turn, alongside the
    /// completion chime, with the freshly finished row ranked to the top and
    /// highlighted. Set false to keep the sound but not the interruption.
    pub open_on_finish: bool,
    /// How many session rows the list shows before it starts scrolling. The
    /// panel is capped at a third of the monitor height regardless, so a few
    /// tall rows (an approval card is several times an idle one) can put the
    /// scrollbar in before this many rows fit. `0` drops the row limit and
    /// leaves only that height cap.
    pub max_visible: usize,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct T3Config {
    /// Track the agents T3 Code runs for its threads.
    ///
    /// They are launched the way a script would launch one — headless, on
    /// stdio — so vibewatch would otherwise filter them out as programmatic.
    /// Turning this off restores that: T3 threads disappear from the panel.
    pub enabled: bool,
    /// On click, ask T3 Code to open the thread as well as raising its window.
    ///
    /// On, because landing on the thread is the point of the click and every
    /// way it can miss lands somewhere harmless: a build that does not handle
    /// `t3code://threads/<environment>/<thread>` reveals its window, which is
    /// what the click did anyway. Turn it off on a machine with no T3 desktop
    /// app, where the scheme has no handler and the desktop would ask which
    /// application to open it with.
    pub deep_link: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    pub window_class: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct LimitsConfig {
    /// Show how much of each provider's account quota is spent, above the
    /// agent list.
    ///
    /// On means the daemon asks Claude's account endpoint for the figures,
    /// using the OAuth token Claude Code already holds — the only place they
    /// exist, since Claude reports them on a live session stream and persists
    /// nothing. That is the one outbound request vibewatch makes, so it gets a
    /// switch: off leaves the section out and the daemon entirely local.
    /// Codex's half needs no network either way, being on disk already.
    pub enabled: bool,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl Default for T3Config {
    fn default() -> Self {
        Self {
            enabled: true,
            deep_link: true,
        }
    }
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            compositor: "auto".to_string(),
            socket_path: None,
            idle_debounce_ms: 3000,
            hold_ceiling_ms: 300_000,
            approval_debounce_ms: 400,
        }
    }
}

impl Default for SoundConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            approval_needed: "builtin:chime".to_string(),
            idle: "builtin:success".to_string(),
            error: "builtin:alert".to_string(),
        }
    }
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            animate: true,
            animation_ms: 220,
            auto_close: true,
            auto_close_ms: 3000,
            open_on_finish: true,
            max_visible: 5,
        }
    }
}

/// Where vibewatch keeps what must survive a restart: the header toggles, and
/// the account-limits cache. `$XDG_STATE_HOME/vibewatch`, falling back the way
/// the spec says to.
pub fn state_dir() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(|| PathBuf::from("~/.local/state"))
        .join("vibewatch")
}

impl Config {
    /// Returns the path to the config file: `$XDG_CONFIG_HOME/vibewatch/config.toml`
    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("vibewatch")
            .join("config.toml")
    }

    /// How long a session must stay quiet before its finish is announced.
    /// See [`GeneralConfig::idle_debounce_ms`]; zero disables the wait.
    pub fn idle_debounce(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.general.idle_debounce_ms)
    }

    /// How long a turn held open by outstanding sub-agents may stay silent
    /// before it is announced anyway. See [`GeneralConfig::hold_ceiling_ms`];
    /// zero holds forever.
    pub fn hold_ceiling(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.general.hold_ceiling_ms)
    }

    /// How long a permission request must stay outstanding before it is
    /// announced. See [`GeneralConfig::approval_debounce_ms`]; zero announces
    /// on arrival.
    pub fn approval_debounce(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.general.approval_debounce_ms)
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
        assert_eq!(config.general.idle_debounce_ms, 3000);
        assert_eq!(config.general.hold_ceiling_ms, 300_000);
        assert_eq!(config.general.approval_debounce_ms, 400);
        assert!(config.sounds.enabled);
        assert_eq!(config.sounds.approval_needed, "builtin:chime");
        assert_eq!(config.sounds.idle, "builtin:success");
        assert_eq!(config.sounds.error, "builtin:alert");
        assert!(config.panel.animate);
        assert_eq!(config.panel.animation_ms, 220);
        assert!(config.panel.auto_close);
        assert_eq!(config.panel.auto_close_ms, 3000);
        assert!(config.panel.open_on_finish);
        assert_eq!(config.panel.max_visible, 5);
        assert!(config.agents.is_empty());
    }

    #[test]
    fn general_section_without_the_waits_keeps_their_defaults() {
        // Any config.toml written before these fields existed still has a
        // `[general]` section. The container-level `#[serde(default)]` has to
        // fill the gaps from `GeneralConfig::default()` and not from
        // `u64::default()` — 0 would silently mean "announce every Stop" for the
        // debounce and "keep a stalled turn silent forever" for the ceiling,
        // i.e. exactly the two failures these settings exist to remove.
        let config: Config = toml::from_str("[general]\ncompositor = \"hyprland\"\n").unwrap();
        assert_eq!(config.general.compositor, "hyprland");
        assert_eq!(config.general.idle_debounce_ms, 3000);
        assert_eq!(config.general.hold_ceiling_ms, 300_000);
        assert_eq!(config.general.approval_debounce_ms, 400);
    }

    #[test]
    fn test_parse_panel_config() {
        let toml_str = r#"
[panel]
animate = false
animation_ms = 120
auto_close = false
auto_close_ms = 8000
open_on_finish = false
max_visible = 8
"#;
        let config = toml::from_str::<Config>(toml_str).unwrap();
        assert!(!config.panel.animate);
        assert_eq!(config.panel.animation_ms, 120);
        assert!(!config.panel.auto_close);
        assert_eq!(config.panel.auto_close_ms, 8000);
        assert!(!config.panel.open_on_finish);
        assert_eq!(config.panel.max_visible, 8);
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
