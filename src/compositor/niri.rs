use anyhow::{Context, Result};
use tokio::process::Command;

use super::{Compositor, CompositorWindow};

pub struct NiriCompositor;

#[derive(Debug, serde::Deserialize)]
pub(crate) struct NiriWindow {
    pub(crate) id: u64,
    pub(crate) pid: Option<u32>,
    pub(crate) app_id: Option<String>,
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
            })
            .collect())
    }
}
