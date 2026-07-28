pub mod session_row;
pub mod window;

use libadwaita as adw;

use crate::config::PanelConfig;
use crate::session::SessionRegistry;

pub use window::{show, toggle};

/// Pin GSK to the software renderer unless the user asked for another one.
///
/// GTK picks the GPU for its Vulkan/GL renderers on its own, and on a multi-GPU
/// machine it can land on a device the compositor does not composite with — an
/// iGPU that drives no output, say. The dmabuf the panel hands over then cannot
/// be imported, so the layer surface arrives as a solid black rectangle where
/// the session list should be, for the rest of the process's life. Nothing in
/// the daemon can detect or recover from that.
///
/// The GPU buys a 360px overlay nothing anyway: measured over ten open/close
/// cycles of the drawer, cairo costs 760ms of CPU against 1070ms for Vulkan and
/// 1120ms for GL. Software rendering is both the cheapest option and the only
/// one that cannot pick the wrong device — it writes plain shm buffers that
/// every compositor can display.
///
/// Must run before GTK initialises: GSK reads the variable when it realises the
/// first renderer.
pub fn prefer_software_renderer() {
    if std::env::var_os("GSK_RENDERER").is_none() {
        std::env::set_var("GSK_RENDERER", "cairo");
    }
}

/// Create the panel window (hidden). Call from the daemon's GTK `connect_activate`.
/// Returns the window handle so the daemon can toggle its visibility.
pub fn create_panel(
    app: &adw::Application,
    registry: SessionRegistry,
    panel_cfg: PanelConfig,
) -> adw::ApplicationWindow {
    window::build_window(app, registry, panel_cfg)
}
