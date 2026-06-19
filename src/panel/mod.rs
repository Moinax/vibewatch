pub mod session_row;
pub mod window;

use libadwaita as adw;

use crate::config::PanelConfig;
use crate::session::SessionRegistry;

pub use window::{show, toggle};

/// Create the panel window (hidden). Call from the daemon's GTK `connect_activate`.
/// Returns the window handle so the daemon can toggle its visibility.
pub fn create_panel(
    app: &adw::Application,
    registry: SessionRegistry,
    panel_cfg: PanelConfig,
) -> adw::ApplicationWindow {
    window::build_window(app, registry, panel_cfg)
}
