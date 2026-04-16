use gtk4 as gtk;
use gtk::prelude::*;

use crate::session::{describe_tool, parent_pid, Session, SessionStatus};

/// Build a ListBoxRow widget for a single session.
///
/// Active (executing/thinking/approval): name + badges + description + action line
/// Idle (compact): name + badges only
pub fn build_row(session: &Session) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("session-row");
    row.set_activatable(false);

    let card = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    card.add_css_class("card-box");
    card.set_margin_start(12);
    card.set_margin_end(12);
    card.set_margin_top(10);
    card.set_margin_bottom(10);

    let indicator = gtk::Label::new(Some("\u{25cf}"));
    indicator.add_css_class("indicator");
    indicator.add_css_class(session.status.css_class());
    indicator.set_valign(gtk::Align::Start);
    indicator.set_margin_top(3);
    card.append(&indicator);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
    content.set_hexpand(true);

    // Header: session name + badges
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 5);

    let name_label = gtk::Label::new(Some(&session.display_name()));
    name_label.add_css_class("session-name");
    name_label.set_hexpand(true);
    name_label.set_halign(gtk::Align::Start);
    name_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name_label.set_max_width_chars(22);
    header.append(&name_label);

    let agent_badge = gtk::Label::new(Some(session.agent.short_name()));
    agent_badge.add_css_class("pill-badge");
    agent_badge.add_css_class("agent-badge");
    header.append(&agent_badge);

    let terminal = session.terminal.as_deref().unwrap_or("Term");
    let term_badge = gtk::Label::new(Some(terminal));
    term_badge.add_css_class("pill-badge");
    term_badge.add_css_class("terminal-badge");
    header.append(&term_badge);

    let time_label = gtk::Label::new(Some(&format_elapsed(session)));
    time_label.add_css_class("pill-badge");
    time_label.add_css_class("time-badge");
    header.append(&time_label);

    content.append(&header);

    let desc_label = gtk::Label::new(Some(&status_description(session)));
    desc_label.add_css_class("status-desc");
    desc_label.set_halign(gtk::Align::Start);
    desc_label.set_hexpand(true);
    desc_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    content.append(&desc_label);

    if let Some(action_text) = action_line(session) {
        let action_label = gtk::Label::new(Some(&action_text));
        action_label.add_css_class("action-line");
        action_label.add_css_class(session.status.css_class());
        action_label.set_halign(gtk::Align::Start);
        action_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        content.append(&action_label);
    }

    card.append(&content);

    let pid = session.pid;
    let window_id = session.window_id.clone();
    let gesture = gtk::GestureClick::new();
    gesture.connect_released(move |_, _, _, _| {
        let wid = window_id.clone();
        let p = pid;
        std::thread::spawn(move || {
            focus_session(wid.as_deref(), p);
        });
    });
    row.add_controller(gesture);

    row.set_child(Some(&card));
    row
}

fn format_elapsed(session: &Session) -> String {
    if let Some(epoch) = session.started_at_epoch {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let secs = now.saturating_sub(epoch);
        if secs < 60 {
            format!("{}s", secs)
        } else if secs < 3600 {
            format!("{}m", secs / 60)
        } else {
            format!("{}h", secs / 3600)
        }
    } else {
        "\u{2014}".to_string()
    }
}

fn status_description(session: &Session) -> String {
    let prompt_ctx = session.last_prompt.as_deref().map(|p| {
        let first_line = p.lines().next().unwrap_or(p);
        if first_line.len() > 50 {
            format!("You: \"{}...\"", &first_line[..47])
        } else {
            format!("You: \"{}\"", first_line)
        }
    });

    // Prompt context takes priority for most statuses
    if let Some(ctx) = &prompt_ctx {
        if !matches!(session.status, SessionStatus::WaitingApproval | SessionStatus::Stopped) {
            return ctx.clone();
        }
    }

    match session.status {
        SessionStatus::Executing => {
            session.current_tool.as_deref()
                .map(|t| format!("Executing {}", t))
                .unwrap_or_else(|| "Executing...".to_string())
        }
        SessionStatus::Thinking => {
            match (&session.last_tool, &session.last_tool_detail) {
                (Some(tool), Some(detail)) => format!("After {}", describe_tool(tool, detail, true)),
                (Some(tool), None) => format!("After {}", tool),
                _ => "Thinking...".to_string(),
            }
        }
        SessionStatus::WaitingApproval => {
            session.current_tool.as_deref()
                .map(|t| format!("Needs approval: {}", t))
                .unwrap_or_else(|| "Waiting for approval".to_string())
        }
        SessionStatus::Idle | SessionStatus::Running => "Idle".to_string(),
        SessionStatus::Stopped => "Stopped".to_string(),
    }
}

fn action_line(session: &Session) -> Option<String> {
    if let (Some(tool), Some(detail)) = (&session.current_tool, &session.tool_detail) {
        return Some(describe_tool(tool, detail, true));
    }
    if session.status == SessionStatus::Thinking {
        if let (Some(tool), Some(detail)) = (&session.last_tool, &session.last_tool_detail) {
            return Some(describe_tool(tool, detail, false));
        }
    }
    None
}

fn focus_session(window_id: Option<&str>, pid: u32) {
    if let Some(wid) = window_id {
        let _ = std::process::Command::new("hyprctl")
            .args(["dispatch", "focuswindow", &format!("address:{wid}")])
            .status();
        return;
    }

    if pid > 0 {
        let mut current_pid = pid;
        for _ in 0..10 {
            if let Ok(output) = std::process::Command::new("hyprctl")
                .args(["dispatch", "focuswindow", &format!("pid:{current_pid}")])
                .output()
            {
                if String::from_utf8_lossy(&output.stdout).trim() == "ok" {
                    return;
                }
            }
            match parent_pid(current_pid) {
                Some(ppid) => current_pid = ppid,
                None => break,
            }
        }
        let _ = std::process::Command::new("niri")
            .args(["msg", "action", "focus-window", "--pid", &pid.to_string()])
            .status();
    }
}
