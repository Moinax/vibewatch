use anyhow::{Context, Result};
use tokio::process::Command;

use super::{Compositor, CompositorWindow};

pub struct HyprlandCompositor;

#[derive(Debug, serde::Deserialize)]
struct HyprClient {
    address: String,
    pid: u32,
    class: String,
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
            })
            .collect())
    }
}
