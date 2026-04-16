pub mod session_row;
pub mod window;

use libadwaita as adw;

use crate::session::SessionRegistry;

/// Create the panel window (hidden). Call from the daemon's GTK `connect_activate`.
/// Returns the window handle so the daemon can toggle its visibility.
pub fn create_panel(app: &adw::Application, registry: SessionRegistry) -> adw::ApplicationWindow {
    window::build_window(app, registry)
}
