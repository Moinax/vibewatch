use gtk4 as gtk;
use gtk4_layer_shell::LayerShell;
use libadwaita as adw;

use adw::prelude::*;

use crate::session::SessionRegistry;

use super::session_row;

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
    window.set_margin(gtk4_layer_shell::Edge::Top, 8);
    window.set_exclusive_zone(0);
    window.set_keyboard_mode(gtk4_layer_shell::KeyboardMode::OnDemand);
    window.set_namespace(Some("vibewatch"));

    // Load CSS
    let css_provider = gtk::CssProvider::new();
    css_provider.load_from_string(include_str!("../../assets/style.css"));
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().unwrap(),
        &css_provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    // Main layout box
    let main_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    main_box.add_css_class("main-box");
    main_box.set_vexpand(false);

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
    let last_snapshot: std::rc::Rc<std::cell::RefCell<String>> =
        std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    gtk::glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
        // Skip polling when hidden
        if !win_ref.is_visible() {
            // Clear snapshot so we rebuild immediately when shown again
            *last_snapshot.borrow_mut() = String::new();
            return gtk::glib::ControlFlow::Continue;
        }

        let sessions = registry.all();
        let snapshot = serde_json::to_string(&sessions).unwrap_or_default();
        let mut prev = last_snapshot.borrow_mut();
        if *prev != snapshot {
            *prev = snapshot;
            drop(prev);
            rebuild_list(&list_ref, &sessions);
            // Resize window height to match content
            let win = win_ref.clone();
            gtk::glib::idle_add_local_once(move || {
                if let Some(content) = win.content() {
                    let (_, natural) = content.preferred_size();
                    let h = natural.height().max(1);
                    win.set_size_request(360, h);
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
