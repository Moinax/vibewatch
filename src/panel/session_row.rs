use gtk4 as gtk;
use gtk::prelude::*;

use crate::session::{Session, SessionStatus};

/// Build a ListBoxRow widget for a single session.
pub fn build_row(session: &Session) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("session-row");

    let outer_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    outer_box.set_margin_start(12);
    outer_box.set_margin_end(8);
    outer_box.set_margin_top(8);
    outer_box.set_margin_bottom(8);

    // Left side: info
    let info_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    info_box.set_hexpand(true);

    // Top line: indicator + agent name + status badge
    let top_line = gtk::Box::new(gtk::Orientation::Horizontal, 6);

    let indicator = gtk::Label::new(Some(status_indicator(session.status)));
    indicator.add_css_class("indicator");
    indicator.add_css_class(session.status.css_class());
    top_line.append(&indicator);

    let agent_label = gtk::Label::new(Some(session.agent.display_name()));
    agent_label.add_css_class("agent-name");
    agent_label.set_hexpand(true);
    agent_label.set_halign(gtk::Align::Start);
    top_line.append(&agent_label);

    let badge = gtk::Label::new(Some(status_text(session.status)));
    badge.add_css_class("status-badge");
    badge.add_css_class(session.status.css_class());
    top_line.append(&badge);

    info_box.append(&top_line);

    // Detail line: tool + detail (if any)
    if let Some(ref tool) = session.current_tool {
        let detail_text = match &session.tool_detail {
            Some(detail) => format!("\u{2514} {tool}: {detail}"),
            None => format!("\u{2514} {tool}"),
        };
        let detail_label = gtk::Label::new(Some(&detail_text));
        detail_label.add_css_class("tool-detail");
        detail_label.set_halign(gtk::Align::Start);
        detail_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        info_box.append(&detail_label);
    }

    outer_box.append(&info_box);

    // Jump button (if we have a window_id or pid > 0)
    let can_focus = session.window_id.is_some() || session.pid > 0;
    if can_focus {
        let jump_button = gtk::Button::with_label("\u{279c}");
        jump_button.add_css_class("jump-button");
        jump_button.set_valign(gtk::Align::Center);

        let window_id = session.window_id.clone();
        let pid = session.pid;
        jump_button.connect_clicked(move |_| {
            let wid = window_id.clone();
            std::thread::spawn(move || {
                focus_session(wid.as_deref(), pid);
            });
        });

        outer_box.append(&jump_button);
    }

    row.set_child(Some(&outer_box));
    row
}

/// Returns the indicator character for a session status.
fn status_indicator(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Executing | SessionStatus::Running | SessionStatus::Thinking => "\u{25cf}",
        _ => "\u{25cb}",
    }
}

/// Returns human-readable status text.
fn status_text(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Thinking => "thinking",
        SessionStatus::Executing => "executing",
        SessionStatus::WaitingApproval => "attention",
        SessionStatus::Idle => "idle",
        SessionStatus::Running => "running",
        SessionStatus::Stopped => "stopped",
    }
}

/// Focus the agent's window via the compositor. Runs in a spawned thread.
fn focus_session(window_id: Option<&str>, pid: u32) {
    // Try hyprctl first (most common in this project's context)
    if let Some(wid) = window_id {
        let _ = std::process::Command::new("hyprctl")
            .args(["dispatch", "focuswindow", &format!("address:{wid}")])
            .status();
        return;
    }

    if pid > 0 {
        // Try hyprctl by pid
        let result = std::process::Command::new("hyprctl")
            .args(["dispatch", "focuswindow", &format!("pid:{pid}")])
            .status();

        if result.is_ok() {
            return;
        }

        // Fallback: try niri
        let _ = std::process::Command::new("niri")
            .args(["msg", "action", "focus-window", "--pid", &pid.to_string()])
            .status();
    }
}
