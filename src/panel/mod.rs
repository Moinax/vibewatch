pub mod limits;
pub mod session_row;
pub mod window;

use gtk4 as gtk;
use libadwaita as adw;

use crate::config::PanelConfig;
use crate::session::SessionRegistry;

pub use window::{show, toggle};

/// Rasterise one of the embedded SVG marks to a `px`-wide image.
///
/// `None` when the SVG cannot be decoded, which means no gdk-pixbuf SVG loader
/// (librsvg) on the system. Every caller then falls back to text: a missing
/// decoration must never be the reason a panel fails to build.
pub(crate) fn svg_mark(svg: &'static [u8], px: i32) -> Option<gtk::Image> {
    let stream = gtk::gio::MemoryInputStream::from_bytes(&gtk::glib::Bytes::from_static(svg));
    // Rasterised at 2x the display size so it stays clean on a HiDPI output.
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_stream_at_scale(
        &stream,
        px * 2,
        px * 2,
        true,
        gtk::gio::Cancellable::NONE,
    )
    .ok()?;
    let image = gtk::Image::from_paintable(Some(&gtk::gdk::Texture::for_pixbuf(&pixbuf)));
    image.set_pixel_size(px);
    Some(image)
}

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
