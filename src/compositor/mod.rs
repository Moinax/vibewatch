pub mod hyprland;
pub mod niri;

use anyhow::{bail, Result};

/// A window discovered by the compositor
#[derive(Debug, Clone)]
pub struct CompositorWindow {
    pub id: String,
    pub pid: u32,
    pub app_id: String,
    pub title: String,
    pub workspace: String,
}

/// Abstraction over compositor IPC
#[async_trait::async_trait]
pub trait Compositor: Send + Sync {
    async fn list_windows(&self) -> Result<Vec<CompositorWindow>>;
    async fn focus_window(&self, window_id: &str) -> Result<()>;

    async fn focus_by_pid(&self, pid: u32) -> Result<()> {
        let windows = self.list_windows().await?;
        let window = windows
            .iter()
            .find(|w| w.pid == pid)
            .ok_or_else(|| anyhow::anyhow!("no window with pid {pid}"))?;
        self.focus_window(&window.id).await
    }

    async fn focus_by_class(&self, class: &str) -> Result<()> {
        let windows = self.list_windows().await?;
        let window = windows
            .iter()
            .find(|w| w.app_id == class)
            .ok_or_else(|| anyhow::anyhow!("no window with class {class}"))?;
        self.focus_window(&window.id).await
    }

    async fn find_by_class(&self, class: &str) -> Result<Vec<CompositorWindow>> {
        let windows = self.list_windows().await?;
        Ok(windows
            .into_iter()
            .filter(|w| w.app_id == class)
            .collect())
    }

    async fn find_by_pid(&self, pid: u32) -> Result<Option<CompositorWindow>> {
        let windows = self.list_windows().await?;
        Ok(windows.into_iter().find(|w| w.pid == pid))
    }
}

/// Detect compositor from environment variables
pub fn detect_compositor() -> Option<String> {
    if let Ok(desktop) = std::env::var("XDG_CURRENT_DESKTOP") {
        let lower = desktop.to_lowercase();
        if lower.contains("hyprland") {
            return Some("hyprland".to_string());
        }
        if lower.contains("niri") {
            return Some("niri".to_string());
        }
    }

    if std::env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok() {
        return Some("hyprland".to_string());
    }

    if std::env::var("NIRI_SOCKET").is_ok() {
        return Some("niri".to_string());
    }

    None
}

/// Create a compositor backend by name
pub fn create_compositor(name: &str) -> Result<Box<dyn Compositor>> {
    match name {
        "hyprland" => Ok(Box::new(hyprland::HyprlandCompositor)),
        "niri" => Ok(Box::new(niri::NiriCompositor)),
        "auto" => {
            let detected = detect_compositor()
                .ok_or_else(|| anyhow::anyhow!("could not detect compositor"))?;
            create_compositor(&detected)
        }
        other => bail!("unknown compositor: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Env-var tests must run sequentially since they share process-wide state.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_clean_env<F: FnOnce() -> R, R>(f: F) -> R {
        let _guard = ENV_LOCK.lock().unwrap();
        // Clear all compositor env vars before each test
        std::env::remove_var("XDG_CURRENT_DESKTOP");
        std::env::remove_var("HYPRLAND_INSTANCE_SIGNATURE");
        std::env::remove_var("NIRI_SOCKET");
        let result = f();
        // Clean up after
        std::env::remove_var("XDG_CURRENT_DESKTOP");
        std::env::remove_var("HYPRLAND_INSTANCE_SIGNATURE");
        std::env::remove_var("NIRI_SOCKET");
        result
    }

    #[test]
    fn test_detect_hyprland_via_env() {
        with_clean_env(|| {
            std::env::set_var("HYPRLAND_INSTANCE_SIGNATURE", "test123");
            assert_eq!(detect_compositor(), Some("hyprland".to_string()));
        });
    }

    #[test]
    fn test_detect_niri_via_env() {
        with_clean_env(|| {
            std::env::set_var("NIRI_SOCKET", "/tmp/niri.sock");
            assert_eq!(detect_compositor(), Some("niri".to_string()));
        });
    }

    #[test]
    fn test_detect_via_xdg_desktop() {
        with_clean_env(|| {
            std::env::set_var("XDG_CURRENT_DESKTOP", "Hyprland");
            assert_eq!(detect_compositor(), Some("hyprland".to_string()));
        });
    }
}
