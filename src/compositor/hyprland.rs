use anyhow::{Context, Result};
use tokio::process::Command;

use super::{Compositor, CompositorWindow};

pub struct HyprlandCompositor;

#[derive(Debug, serde::Deserialize)]
struct HyprClient {
    address: String,
    pid: u32,
    class: String,
    title: String,
    workspace: HyprWorkspace,
}

#[derive(Debug, serde::Deserialize)]
struct HyprWorkspace {
    name: String,
}

#[async_trait::async_trait]
impl Compositor for HyprlandCompositor {
    async fn list_windows(&self) -> Result<Vec<CompositorWindow>> {
        let output = Command::new("hyprctl")
            .args(["clients", "-j"])
            .output()
            .await
            .context("failed to run hyprctl")?;

        let clients: Vec<HyprClient> = serde_json::from_slice(&output.stdout)
            .context("failed to parse hyprctl output")?;

        Ok(clients
            .into_iter()
            .map(|c| CompositorWindow {
                id: c.address,
                pid: c.pid,
                app_id: c.class,
                title: c.title,
                workspace: c.workspace.name,
            })
            .collect())
    }

    async fn focus_window(&self, window_id: &str) -> Result<()> {
        let status = Command::new("hyprctl")
            .args(["dispatch", "focuswindow", &format!("address:{window_id}")])
            .status()
            .await
            .context("failed to run hyprctl dispatch")?;

        if !status.success() {
            anyhow::bail!("hyprctl dispatch focuswindow failed");
        }
        Ok(())
    }

    async fn focus_by_pid(&self, pid: u32) -> Result<()> {
        let status = Command::new("hyprctl")
            .args(["dispatch", "focuswindow", &format!("pid:{pid}")])
            .status()
            .await
            .context("failed to run hyprctl dispatch")?;

        if !status.success() {
            anyhow::bail!("hyprctl dispatch focuswindow by pid failed");
        }
        Ok(())
    }
}
