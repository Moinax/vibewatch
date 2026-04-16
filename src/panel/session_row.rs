use gtk4 as gtk;
use gtk::prelude::*;

use crate::session::{Session, SessionStatus};

/// Build a ListBoxRow widget for a single session.
///
/// Active layout (executing/thinking/approval):
/// ┌──────────────────────────────────────────────────┐
/// │  ●  VibeWatch              Claude  Kitty   27m   │
/// │     You: "fix the auth bug"                      │
/// │     Writing middleware.ts                         │
/// └──────────────────────────────────────────────────┘
///
/// Idle layout (compact):
/// ┌──────────────────────────────────────────────────┐
/// │  ●  dotfiles               Claude  Kitty    1h   │
/// └──────────────────────────────────────────────────┘
pub fn build_row(session: &Session) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("session-row");
    row.set_activatable(false);

    let is_active = matches!(
        session.status,
        SessionStatus::Executing | SessionStatus::Thinking | SessionStatus::WaitingApproval
    );

    let card = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    card.add_css_class("card-box");
    card.set_margin_start(12);
    card.set_margin_end(12);
    card.set_margin_top(if is_active { 10 } else { 8 });
    card.set_margin_bottom(if is_active { 10 } else { 8 });

    // Indicator dot
    let indicator = gtk::Label::new(Some("\u{25cf}"));
    indicator.add_css_class("indicator");
    indicator.add_css_class(session.status.css_class());
    indicator.set_valign(gtk::Align::Start);
    indicator.set_margin_top(3);
    card.append(&indicator);

    // Content area
    let content = gtk::Box::new(gtk::Orientation::Vertical, if is_active { 2 } else { 0 });
    content.set_hexpand(true);

    // Row 1: session name + badges
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 5);

    let name_label = gtk::Label::new(Some(&session.display_name()));
    name_label.add_css_class("session-name");
    name_label.set_hexpand(true);
    name_label.set_halign(gtk::Align::Start);
    name_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name_label.set_max_width_chars(22);
    header.append(&name_label);

    // Agent badge
    let agent_badge = gtk::Label::new(Some(session.agent.short_name()));
    agent_badge.add_css_class("pill-badge");
    agent_badge.add_css_class("agent-badge");
    header.append(&agent_badge);

    // Terminal badge
    let terminal = detect_terminal(session.pid);
    let term_badge = gtk::Label::new(Some(&terminal));
    term_badge.add_css_class("pill-badge");
    term_badge.add_css_class("terminal-badge");
    header.append(&term_badge);

    // Elapsed time
    let elapsed = format_elapsed(session);
    let time_label = gtk::Label::new(Some(&elapsed));
    time_label.add_css_class("pill-badge");
    time_label.add_css_class("time-badge");
    header.append(&time_label);

    content.append(&header);

    // For active sessions: show description + action line
    if is_active {
        // Row 2: status description with context
        let desc_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);

        let desc_text = status_description(session);
        let desc_label = gtk::Label::new(Some(&desc_text));
        desc_label.add_css_class("status-desc");
        desc_label.set_halign(gtk::Align::Start);
        desc_label.set_hexpand(true);
        desc_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        desc_box.append(&desc_label);

        content.append(&desc_box);

        // Row 3: current action (colored, link-style)
        if let Some(action_text) = action_line(session) {
            let action_label = gtk::Label::new(Some(&action_text));
            action_label.add_css_class("action-line");
            action_label.add_css_class(session.status.css_class());
            action_label.set_halign(gtk::Align::Start);
            action_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            content.append(&action_label);
        }
    }

    card.append(&content);

    // Make the whole row clickable to jump
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
        "\u{2014}".to_string()
    }
}

/// Build the status description line.
fn status_description(session: &Session) -> String {
    let prompt_ctx = session.last_prompt.as_deref().map(|p| {
        let first_line = p.lines().next().unwrap_or(p);
        if first_line.len() > 50 {
            format!("You: \"{}...\"", &first_line[..47])
        } else {
            format!("You: \"{}\"", first_line)
        }
    });

    match session.status {
        SessionStatus::Executing => {
            if let Some(ctx) = &prompt_ctx {
                ctx.clone()
            } else if let Some(ref tool) = session.current_tool {
                format!("Executing {}", tool)
            } else {
                "Executing...".to_string()
            }
        }
        SessionStatus::Thinking => {
            if let Some(ctx) = &prompt_ctx {
                ctx.clone()
            } else if let Some(ref tool) = session.last_tool {
                let detail = session.last_tool_detail.as_deref().unwrap_or("");
                if detail.is_empty() {
                    format!("After {}", tool)
                } else {
                    format!("After {}", format_tool_action(tool, detail))
                }
            } else {
                "Thinking...".to_string()
            }
        }
        SessionStatus::WaitingApproval => {
            if let Some(ref tool) = session.current_tool {
                format!("Needs approval: {}", tool)
            } else {
                "Waiting for approval".to_string()
            }
        }
        SessionStatus::Idle | SessionStatus::Running => {
            if let Some(ctx) = &prompt_ctx {
                ctx.clone()
            } else {
                "Idle".to_string()
            }
        }
        SessionStatus::Stopped => "Stopped".to_string(),
    }
}

/// Format a tool + detail into a human-readable past-tense action.
fn format_tool_action(tool: &str, detail: &str) -> String {
    match tool {
        "Write" => format!("writing {}", detail),
        "Edit" => format!("editing {}", detail),
        "Read" => format!("reading {}", detail),
        "Bash" => detail.to_string(),
        "Grep" | "Glob" => format!("searching {}", detail),
        _ => format!("{} {}", tool, detail),
    }
}

/// Build the action line (e.g. "Writing src/main.rs") -- shown during active work.
fn action_line(session: &Session) -> Option<String> {
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
    // When thinking, show what was last done
    if session.status == SessionStatus::Thinking {
        if let (Some(tool), Some(detail)) = (&session.last_tool, &session.last_tool_detail) {
            let action = match tool.as_str() {
                "Write" => format!("Wrote {}", detail),
                "Edit" => format!("Edited {}", detail),
                "Read" => format!("Read {}", detail),
                "Bash" => detail.to_string(),
                "Grep" | "Glob" => format!("Searched {}", detail),
                _ => format!("{}: {}", tool, detail),
            };
            return Some(action);
        }
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

        let _ = std::process::Command::new("niri")
            .args(["msg", "action", "focus-window", "--pid", &pid.to_string()])
            .status();
    }
}
