use std::hash::{Hash, Hasher};

use gtk4 as gtk;
use gtk4_layer_shell::LayerShell;
use libadwaita as adw;

use adw::prelude::*;

use crate::session::{Session, SessionRegistry};

use super::session_row;

/// Hash the panel-visible fields of every session. The 10 Hz timer uses this
/// to skip rebuilds when nothing the panel renders has changed — far cheaper
/// than the previous full-JSON-serialize-and-compare.
fn sessions_fingerprint(sessions: &[Session]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    sessions.len().hash(&mut h);
    for s in sessions {
        s.id.hash(&mut h);
        s.status.hash(&mut h);
        s.current_tool.hash(&mut h);
        s.tool_detail.hash(&mut h);
        s.last_tool.hash(&mut h);
        s.last_tool_detail.hash(&mut h);
        s.last_tool_at.hash(&mut h);
        s.last_prompt.hash(&mut h);
        s.last_prompt_at.hash(&mut h);
        s.last_agent_text.hash(&mut h);
        s.last_agent_text_at.hash(&mut h);
        s.session_name.hash(&mut h);
        s.terminal.hash(&mut h);
        s.started_at_epoch.hash(&mut h);
        s.pending_approval
            .as_ref()
            .map(|p| (&p.request_id, p.choices.len()))
            .hash(&mut h);
    }
    h.finish()
}

pub fn build_window(app: &adw::Application, registry: SessionRegistry) -> adw::ApplicationWindow {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("vibewatch")
        .build();
    // Set only width, let height be driven by content
    window.set_size_request(360, 1);

    // Layer shell setup — anchor top only so the compositor centers us horizontally.
    window.init_layer_shell();
    window.set_layer(gtk4_layer_shell::Layer::Overlay);
    window.set_anchor(gtk4_layer_shell::Edge::Top, true);
    window.set_margin(gtk4_layer_shell::Edge::Top, 14);
    window.set_exclusive_zone(0);
    // `None` so the layer surface never steals keyboard focus from the
    // focused terminal. The panel is mouse-only (GestureClick on rows,
    // connect_clicked on buttons) — no widgets consume keyboard input.
    window.set_keyboard_mode(gtk4_layer_shell::KeyboardMode::None);
    window.set_namespace(Some("vibewatch"));

    // Load CSS — palette provider is swapped on OS dark/light theme change.
    let display = gtk::gdk::Display::default().unwrap();

    let palette_provider = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(
        &display,
        &palette_provider,
        gtk::STYLE_PROVIDER_PRIORITY_USER,
    );

    let style_provider = gtk::CssProvider::new();
    style_provider.load_from_string(include_str!("../../assets/style.css"));
    gtk::style_context_add_provider_for_display(
        &display,
        &style_provider,
        gtk::STYLE_PROVIDER_PRIORITY_USER,
    );

    const PALETTE_MOCHA: &str = include_str!("../../assets/palette-mocha.css");
    const PALETTE_LATTE: &str = include_str!("../../assets/palette-latte.css");
    let load_palette = |provider: &gtk::CssProvider, dark: bool| {
        provider.load_from_string(if dark { PALETTE_MOCHA } else { PALETTE_LATTE });
    };

    let style_manager = adw::StyleManager::default();
    load_palette(&palette_provider, style_manager.is_dark());
    let palette_for_notify = palette_provider.clone();
    style_manager.connect_dark_notify(move |sm| {
        load_palette(&palette_for_notify, sm.is_dark());
    });

    // Main layout box
    let main_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    main_box.add_css_class("main-box");
    main_box.set_vexpand(false);
    main_box.set_size_request(360, -1);
    main_box.set_hexpand(false);
    main_box.set_halign(gtk::Align::Center);

    // Session list
    let session_list = gtk::ListBox::new();
    session_list.set_selection_mode(gtk::SelectionMode::None);
    session_list.add_css_class("session-list");

    let empty_label = gtk::Label::new(Some("No agents running"));
    empty_label.add_css_class("empty-state");
    session_list.set_placeholder(Some(&empty_label));

    main_box.append(&session_list);
    window.set_content(Some(&main_box));

    // Poll registry every 500ms, only rebuild if data changed
    // Skip polling when window is hidden to avoid unnecessary work
    let list_ref = session_list;
    let win_ref = window.clone();
    // `None` means "rebuild on next tick" — used when the window was just
    // shown so we always repaint from a fresh registry read.
    let last_fingerprint: std::rc::Rc<std::cell::RefCell<Option<u64>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    gtk::glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        if !win_ref.is_visible() {
            *last_fingerprint.borrow_mut() = None;
            return gtk::glib::ControlFlow::Continue;
        }

        let sessions = registry.all();
        let fp = sessions_fingerprint(&sessions);
        let mut prev = last_fingerprint.borrow_mut();
        if *prev != Some(fp) {
            *prev = Some(fp);
            drop(prev);
            rebuild_list(&list_ref, &sessions);
            // Resize window height to match content
            let win = win_ref.clone();
            gtk::glib::idle_add_local_once(move || {
                if let Some(content) = win.content() {
                    let (_, natural) = content.preferred_size();
                    let h = natural.height().max(1);
                    // set_default_size is the knob that actually shrinks a GTK
                    // window below a previous allocation; set_size_request only
                    // pins the minimum, which otherwise keeps the surface wide
                    // after a transient wide row (e.g. a 3-button approval bar).
                    win.set_size_request(360, h);
                    win.set_default_size(360, h);
                }
            });
        }
        gtk::glib::ControlFlow::Continue
    });

    // Start hidden — daemon will toggle visibility via IPC
    window.set_visible(false);

    window
}

/// Rebuild the list from scratch with new session data.
fn rebuild_list(list: &gtk::ListBox, sessions: &[crate::session::Session]) {
    while let Some(row) = list.row_at_index(0) {
        list.remove(&row);
    }
    for session in sessions {
        let row = session_row::build_row(session);
        list.append(&row);
    }
}
