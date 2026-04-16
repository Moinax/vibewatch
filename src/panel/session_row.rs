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

    let agent_label = gtk::Label::new(Some(&session.display_name()));
    agent_label.add_css_class("agent-name");
    agent_label.set_hexpand(true);
    agent_label.set_halign(gtk::Align::Start);
    agent_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
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
            eprintln!("vibewatch: jump clicked for pid={} wid={:?}", pid, window_id);
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
    if let Some(wid) = window_id {
        let _ = std::process::Command::new("hyprctl")
            .args(["dispatch", "focuswindow", &format!("address:{wid}")])
            .status();
        return;
    }

    if pid > 0 {
        // Walk up process tree to find the terminal window
        let mut current_pid = pid;
        for _ in 0..10 {  // max 10 levels up
            let result = std::process::Command::new("hyprctl")
                .args(["dispatch", "focuswindow", &format!("pid:{current_pid}")])
                .output();

            if let Ok(output) = result {
                let stdout = String::from_utf8_lossy(&output.stdout);
                eprintln!("vibewatch: hyprctl pid:{} => '{}'", current_pid, stdout.trim());
                if stdout.trim() == "ok" {
                    eprintln!("vibewatch: focused window at pid={}", current_pid);
                    return;
                }
            }

            // Read parent PID from /proc/{pid}/stat
            match std::fs::read_to_string(format!("/proc/{current_pid}/stat")) {
                Ok(stat) => {
                    // Format: "pid (comm) state ppid ..."
                    // Find the closing paren, then parse ppid
                    if let Some(after_paren) = stat.rfind(')') {
                        let rest = &stat[after_paren + 2..]; // skip ") "
                        let fields: Vec<&str> = rest.split_whitespace().collect();
                        if let Some(ppid_str) = fields.get(1) {
                            if let Ok(ppid) = ppid_str.parse::<u32>() {
                                if ppid <= 1 { break; } // reached init
                                current_pid = ppid;
                                continue;
                            }
                        }
                    }
                    break;
                }
                Err(_) => break,
            }
        }

        // Fallback: try niri
        let _ = std::process::Command::new("niri")
            .args(["msg", "action", "focus-window", "--pid", &pid.to_string()])
            .status();
    }
}
