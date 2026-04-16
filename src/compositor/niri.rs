use anyhow::{Context, Result};
use tokio::process::Command;

use super::{Compositor, CompositorWindow};

pub struct NiriCompositor;

#[derive(Debug, serde::Deserialize)]
struct NiriWindow {
    id: u64,
    pid: Option<u32>,
    app_id: Option<String>,
    title: Option<String>,
    workspace_id: Option<u64>,
}

#[async_trait::async_trait]
impl Compositor for NiriCompositor {
    async fn list_windows(&self) -> Result<Vec<CompositorWindow>> {
        let output = Command::new("niri")
            .args(["msg", "-j", "windows"])
            .output()
            .await
            .context("failed to run niri msg")?;

        let windows: Vec<NiriWindow> = serde_json::from_slice(&output.stdout)
            .context("failed to parse niri msg output")?;

        Ok(windows
            .into_iter()
            .map(|w| CompositorWindow {
                id: w.id.to_string(),
                pid: w.pid.unwrap_or(0),
                app_id: w.app_id.unwrap_or_default(),
                title: w.title.unwrap_or_default(),
                workspace: w
                    .workspace_id
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
            })
            .collect())
    }

    async fn focus_window(&self, window_id: &str) -> Result<()> {
        let status = Command::new("niri")
            .args(["msg", "action", "focus-window", "--id", window_id])
            .status()
            .await
            .context("failed to run niri msg action")?;

        if !status.success() {
            anyhow::bail!("niri msg action focus-window failed");
        }
        Ok(())
    }
}
