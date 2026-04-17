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

    let desc_label = gtk::Label::new(Some(&describe(session)));
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

/// Maximum characters of prompt/agent text to render before ellipsizing.
const DESCRIBE_MAX_CHARS: usize = 60;

/// Description-line content for a session: latest of user prompt / agent text,
/// with a speaker prefix. Falls back to a status-based string when neither
/// text is available.
pub(crate) fn describe(session: &Session) -> String {
    let user = session.last_prompt.as_deref().zip(session.last_prompt_at);
    let agent = session.last_agent_text.as_deref().zip(session.last_agent_text_at);
    match (user, agent) {
        (Some((p, _)), None) => render_user(p),
        (None, Some((a, _))) => render_agent(session, a),
        (Some((p, pu)), Some((a, au))) => {
            if pu > au {
                render_user(p)
            } else {
                render_agent(session, a)
            }
        }
        (None, None) => fallback_for_status(session.status),
    }
}

fn render_user(text: &str) -> String {
    format!("You: \"{}\"", truncate(text, DESCRIBE_MAX_CHARS))
}

fn render_agent(session: &Session, text: &str) -> String {
    format!(
        "{}: \"{}\"",
        session.agent.short_name(),
        truncate(text, DESCRIBE_MAX_CHARS),
    )
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(3)).collect();
        out.push_str("...");
        out
    }
}

fn fallback_for_status(status: SessionStatus) -> String {
    match status {
        SessionStatus::Idle | SessionStatus::Running => "Idle".into(),
        SessionStatus::Stopped => "Stopped".into(),
        SessionStatus::Thinking => "Thinking...".into(),
        SessionStatus::Executing => "Working...".into(),
        SessionStatus::WaitingApproval => "Awaiting approval".into(),
    }
}

fn action_line(session: &Session) -> Option<String> {
    if session.status == SessionStatus::WaitingApproval {
        let tool = session.current_tool.as_deref().unwrap_or("tool");
        return Some(format!("Needs approval: {}", tool));
    }
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

#[cfg(test)]
mod tests {
    use super::describe;
    use crate::session::{AgentKind, Session, SessionStatus};

    fn mk(agent: AgentKind) -> Session {
        Session::new("s1".into(), agent, 1)
    }

    #[test]
    fn user_only_renders_you_prefix() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.last_prompt = Some("fix the deploy".into());
        s.last_prompt_at = Some(100);
        assert_eq!(describe(&s), "You: \"fix the deploy\"");
    }

    #[test]
    fn agent_only_renders_agent_prefix() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.last_agent_text = Some("Tests pass.".into());
        s.last_agent_text_at = Some(100);
        assert_eq!(describe(&s), "Claude: \"Tests pass.\"");
    }

    #[test]
    fn codex_agent_uses_codex_prefix() {
        let mut s = mk(AgentKind::Codex);
        s.last_agent_text = Some("All good.".into());
        s.last_agent_text_at = Some(100);
        assert_eq!(describe(&s), "Codex: \"All good.\"");
    }

    #[test]
    fn user_wins_when_newer() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.last_prompt = Some("please do X".into());
        s.last_prompt_at = Some(200);
        s.last_agent_text = Some("done".into());
        s.last_agent_text_at = Some(100);
        assert_eq!(describe(&s), "You: \"please do X\"");
    }

    #[test]
    fn agent_wins_when_newer_or_equal() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.last_prompt = Some("please do X".into());
        s.last_prompt_at = Some(100);
        s.last_agent_text = Some("done".into());
        s.last_agent_text_at = Some(100);
        assert_eq!(describe(&s), "Claude: \"done\"");
    }

    #[test]
    fn idle_fallback_when_nothing_captured() {
        let s = mk(AgentKind::ClaudeCode);
        assert_eq!(describe(&s), "Idle");
    }

    #[test]
    fn stopped_fallback() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.status = SessionStatus::Stopped;
        assert_eq!(describe(&s), "Stopped");
    }

    #[test]
    fn executing_fallback_when_no_text() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.status = SessionStatus::Executing;
        assert_eq!(describe(&s), "Working...");
    }

    #[test]
    fn waiting_approval_fallback_when_no_text() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.status = SessionStatus::WaitingApproval;
        assert_eq!(describe(&s), "Awaiting approval");
    }

    #[test]
    fn long_text_is_truncated_with_ellipsis() {
        let mut s = mk(AgentKind::ClaudeCode);
        let long: String = "x".repeat(200);
        s.last_prompt = Some(long.clone());
        s.last_prompt_at = Some(1);
        let out = describe(&s);
        assert!(out.starts_with("You: \""));
        assert!(out.ends_with("...\""));
        assert!(out.len() < long.len() + 20);
    }

    #[test]
    fn action_line_shows_needs_approval_for_waiting() {
        use super::action_line;
        let mut s = mk(AgentKind::ClaudeCode);
        s.status = SessionStatus::WaitingApproval;
        s.current_tool = Some("Bash".into());
        assert_eq!(action_line(&s).as_deref(), Some("Needs approval: Bash"));
    }

    #[test]
    fn action_line_still_shows_live_tool_when_executing() {
        use super::action_line;
        let mut s = mk(AgentKind::ClaudeCode);
        s.status = SessionStatus::Executing;
        s.current_tool = Some("Edit".into());
        s.tool_detail = Some("src/main.rs".into());
        assert_eq!(action_line(&s).as_deref(), Some("Editing src/main.rs"));
    }
}
