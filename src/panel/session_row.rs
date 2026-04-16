use gtk4 as gtk;
use gtk::prelude::*;

use crate::session::{Session, SessionStatus};

/// Build a ListBoxRow widget for a single session, styled like Vibe Island cards.
///
/// Layout:
/// ┌─────────────────────────────────────────────┐
/// │ ● project-name          Claude  Kitty  27m  │
/// │   Status description text                   │
/// │   Writing src/main.rs                       │
/// └─────────────────────────────────────────────┘
pub fn build_row(session: &Session) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("session-row");
    row.set_activatable(true);

    let card = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    card.set_margin_start(14);
    card.set_margin_end(14);
    card.set_margin_top(10);
    card.set_margin_bottom(10);

    // Indicator dot (left side, vertically centered)
    let indicator = gtk::Label::new(Some("\u{25cf}"));
    indicator.add_css_class("indicator");
    indicator.add_css_class(session.status.css_class());
    indicator.set_valign(gtk::Align::Start);
    indicator.set_margin_top(4);
    card.append(&indicator);

    // Content area
    let content = gtk::Box::new(gtk::Orientation::Vertical, 3);
    content.set_hexpand(true);

    // Row 1: project name + badges
    let row1 = gtk::Box::new(gtk::Orientation::Horizontal, 6);

    let project_name = project_label(session);
    project_name.add_css_class("project-name");
    project_name.set_hexpand(true);
    project_name.set_halign(gtk::Align::Start);
    project_name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    row1.append(&project_name);

    // Agent badge
    let agent_badge = gtk::Label::new(Some(session.agent.short_name()));
    agent_badge.add_css_class("pill-badge");
    agent_badge.add_css_class("agent-badge");
    row1.append(&agent_badge);

    // Terminal badge
    let terminal = detect_terminal(session.pid);
    let term_badge = gtk::Label::new(Some(&terminal));
    term_badge.add_css_class("pill-badge");
    term_badge.add_css_class("terminal-badge");
    row1.append(&term_badge);

    // Elapsed time badge
    let elapsed = format_elapsed(session);
    let time_badge = gtk::Label::new(Some(&elapsed));
    time_badge.add_css_class("pill-badge");
    time_badge.add_css_class("time-badge");
    row1.append(&time_badge);

    content.append(&row1);

    // Row 2: status description
    let status_desc = status_description(session);
    let desc_label = gtk::Label::new(Some(&status_desc));
    desc_label.add_css_class("status-desc");
    desc_label.set_halign(gtk::Align::Start);
    desc_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    content.append(&desc_label);

    // Row 3: current action (only if executing with tool info)
    if let Some(action_text) = action_line(session) {
        let action_label = gtk::Label::new(Some(&action_text));
        action_label.add_css_class("action-line");
        action_label.add_css_class(session.status.css_class());
        action_label.set_halign(gtk::Align::Start);
        action_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        content.append(&action_label);
    }

    card.append(&content);

    // Make the whole row clickable to jump
    let pid = session.pid;
    let window_id = session.window_id.clone();

    // Use a GestureClick on the row for jump
    let gesture = gtk::GestureClick::new();
    gesture.connect_released(move |gesture, _, _, _| {
        let wid = window_id.clone();
        let p = pid;
        // Close the panel after jumping
        if let Some(widget) = gesture.widget() {
            if let Some(root) = widget.root() {
                if let Some(window) = root.downcast_ref::<gtk::Window>() {
                    window.close();
                }
            }
        }
        std::thread::spawn(move || {
            focus_session(wid.as_deref(), p);
        });
    });
    row.add_controller(gesture);

    row.set_child(Some(&card));
    row
}

/// Get the project folder name, or fall back to agent name.
fn project_label(session: &Session) -> gtk::Label {
    let name = if let Some(ref cwd) = session.cwd {
        std::path::Path::new(cwd)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string()
    } else {
        // Try /proc for scanned sessions
        std::fs::read_link(format!("/proc/{}/cwd", session.pid))
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| "unknown".to_string())
    };
    gtk::Label::new(Some(&name))
}

/// Detect which terminal the process runs in by walking up the process tree.
fn detect_terminal(pid: u32) -> String {
    let mut current = pid;
    for _ in 0..10 {
        if let Ok(comm) = std::fs::read_to_string(format!("/proc/{}/comm", current)) {
            let comm = comm.trim();
            match comm {
                "kitty" => return "Kitty".to_string(),
                "alacritty" => return "Alacritty".to_string(),
                "foot" => return "Foot".to_string(),
                "wezterm-gui" | "wezterm" => return "WezTerm".to_string(),
                "cursor" => return "Cursor".to_string(),
                "code" => return "VSCode".to_string(),
                "webstorm" | "idea" => return "JetBrains".to_string(),
                _ => {}
            }
        }
        // Walk up
        match std::fs::read_to_string(format!("/proc/{}/stat", current)) {
            Ok(stat) => {
                if let Some(after_paren) = stat.rfind(')') {
                    let rest = &stat[after_paren + 2..];
                    let fields: Vec<&str> = rest.split_whitespace().collect();
                    if let Some(ppid_str) = fields.get(1) {
                        if let Ok(ppid) = ppid_str.parse::<u32>() {
                            if ppid <= 1 { break; }
                            current = ppid;
                            continue;
                        }
                    }
                }
                break;
            }
            Err(_) => break,
        }
    }
    "Term".to_string()
}

/// Format elapsed time since session started.
fn format_elapsed(session: &Session) -> String {
    if let Some(started) = session.started_at {
        let secs = started.elapsed().as_secs();
        if secs < 60 {
            format!("{}s", secs)
        } else if secs < 3600 {
            format!("{}m", secs / 60)
        } else {
            format!("{}h", secs / 3600)
        }
    } else {
        // Fallback: check /proc/{pid}/stat start time
        "—".to_string()
    }
}

/// Build the status description line.
fn status_description(session: &Session) -> String {
    let prompt_ctx = session.last_prompt.as_deref().map(|p| {
        // Show first line, truncated
        let first_line = p.lines().next().unwrap_or(p);
        if first_line.len() > 60 {
            format!("\"{}...\"", &first_line[..57])
        } else {
            format!("\"{}\"", first_line)
        }
    });

    match session.status {
        SessionStatus::Executing => {
            if let Some(ref tool) = session.current_tool {
                format!("Executing {}", tool)
            } else {
                "Executing...".to_string()
            }
        }
        SessionStatus::Thinking => {
            if let Some(ctx) = &prompt_ctx {
                format!("Thinking... — {}", ctx)
            } else if let Some(ref tool) = session.last_tool {
                let detail = session.last_tool_detail.as_deref().unwrap_or("");
                if detail.is_empty() {
                    format!("Thinking... — last: {}", tool)
                } else {
                    format!("Thinking... — last: {}", format_tool_action(tool, detail))
                }
            } else {
                "Thinking...".to_string()
            }
        }
        SessionStatus::WaitingApproval => {
            if let Some(ref tool) = session.current_tool {
                format!("Waiting for approval — {}", tool)
            } else {
                "Waiting for approval".to_string()
            }
        }
        SessionStatus::Idle => {
            if let Some(ctx) = &prompt_ctx {
                format!("Idle — {}", ctx)
            } else if let Some(ref tool) = session.last_tool {
                let detail = session.last_tool_detail.as_deref().unwrap_or("");
                if detail.is_empty() {
                    format!("Idle — last: {}", tool)
                } else {
                    format!("Idle — last: {}", format_tool_action(tool, detail))
                }
            } else {
                "Idle".to_string()
            }
        }
        SessionStatus::Running => "Idle".to_string(),
        SessionStatus::Stopped => "Stopped".to_string(),
    }
}

/// Format a tool + detail into a human-readable action.
fn format_tool_action(tool: &str, detail: &str) -> String {
    match tool {
        "Write" => format!("wrote {}", detail),
        "Edit" => format!("edited {}", detail),
        "Read" => format!("read {}", detail),
        "Bash" => detail.to_string(),
        "Grep" | "Glob" => format!("searched {}", detail),
        _ => format!("{} {}", tool, detail),
    }
}

/// Build the action line (e.g. "Writing src/main.rs") — shown during active work.
fn action_line(session: &Session) -> Option<String> {
    // Show current tool if executing
    if let (Some(tool), Some(detail)) = (&session.current_tool, &session.tool_detail) {
        let action = match tool.as_str() {
            "Write" => format!("Writing {}", detail),
            "Edit" => format!("Editing {}", detail),
            "Read" => format!("Reading {}", detail),
            "Bash" => detail.to_string(),
            "Grep" | "Glob" => format!("Searching {}", detail),
            _ => format!("{}: {}", tool, detail),
        };
        return Some(action);
    }
    None
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
        for _ in 0..10 {
            let result = std::process::Command::new("hyprctl")
                .args(["dispatch", "focuswindow", &format!("pid:{current_pid}")])
                .output();

            if let Ok(output) = result {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.trim() == "ok" {
                    return;
                }
            }

            match std::fs::read_to_string(format!("/proc/{current_pid}/stat")) {
                Ok(stat) => {
                    if let Some(after_paren) = stat.rfind(')') {
                        let rest = &stat[after_paren + 2..];
                        let fields: Vec<&str> = rest.split_whitespace().collect();
                        if let Some(ppid_str) = fields.get(1) {
                            if let Ok(ppid) = ppid_str.parse::<u32>() {
                                if ppid <= 1 { break; }
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
