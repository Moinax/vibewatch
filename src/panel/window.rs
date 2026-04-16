use gtk4 as gtk;
use gtk4_layer_shell::LayerShell;
use libadwaita as adw;

use adw::prelude::*;

use crate::config::Config;
use crate::ipc::{InboundEvent, StatusResponse};
use crate::session::Session;

use super::session_row;

pub fn build_window(app: &adw::Application) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("vibewatch")
        .default_width(360)
        .default_height(-1)
        .build();

    // Layer shell setup
    window.init_layer_shell();
    window.set_layer(gtk4_layer_shell::Layer::Overlay);
    window.set_anchor(gtk4_layer_shell::Edge::Top, true);
    window.set_anchor(gtk4_layer_shell::Edge::Right, true);
    window.set_margin(gtk4_layer_shell::Edge::Top, 8);
    window.set_margin(gtk4_layer_shell::Edge::Right, 8);
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

    // Session list
    let session_list = gtk::ListBox::new();
    session_list.set_selection_mode(gtk::SelectionMode::None);
    session_list.add_css_class("session-list");

    let empty_label = gtk::Label::new(Some("No agents running"));
    empty_label.add_css_class("empty-state");
    session_list.set_placeholder(Some(&empty_label));

    // Scrolled window — no fixed height, grows with content up to max-height (set in CSS)
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .child(&session_list)
        .build();
    scrolled.add_css_class("scroll-container");

    main_box.append(&scrolled);
    window.set_content(Some(&main_box));

    // Poll daemon every 500ms, only rebuild if data changed
    let list_ref = session_list;
    let last_snapshot: std::rc::Rc<std::cell::RefCell<String>> =
        std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    gtk::glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
        let sessions = fetch_sessions();
        let snapshot = serde_json::to_string(&sessions).unwrap_or_default();
        let mut prev = last_snapshot.borrow_mut();
        if *prev != snapshot {
            *prev = snapshot;
            drop(prev);
            rebuild_list(&list_ref, &sessions);
        }
        gtk::glib::ControlFlow::Continue
    });

    window.present();
}

/// Rebuild the list from scratch with new session data.
fn rebuild_list(list: &gtk::ListBox, sessions: &[Session]) {
    while let Some(row) = list.row_at_index(0) {
        list.remove(&row);
    }
    for session in sessions {
        let row = session_row::build_row(session);
        list.append(&row);
    }
}

/// Synchronously connect to the daemon socket, send GetStatus, and parse the response.
fn fetch_sessions() -> Vec<Session> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let config = match Config::load() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let socket_path = config.socket_path();

    let mut stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    // Set a short timeout so the GUI doesn't freeze
    let timeout = std::time::Duration::from_millis(200);
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));

    let event = InboundEvent::GetStatus;
    let mut json = match serde_json::to_string(&event) {
        Ok(j) => j,
        Err(_) => return Vec::new(),
    };
    json.push('\n');

    if stream.write_all(json.as_bytes()).is_err() {
        return Vec::new();
    }
    if stream.flush().is_err() {
        return Vec::new();
    }

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    if reader.read_line(&mut response).is_err() {
        return Vec::new();
    }

    let status: StatusResponse = match serde_json::from_str(response.trim()) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    status.sessions
}
