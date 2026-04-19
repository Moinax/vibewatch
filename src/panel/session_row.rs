use gtk4 as gtk;
use gtk::prelude::*;

use crate::session::{describe_tool, parent_pid, prettify_tool_name, Session, SessionStatus};

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
    name_label.set_halign(gtk::Align::Fill);
    name_label.set_xalign(0.0);
    name_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name_label.set_max_width_chars(1);
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

    if let Some(desc_text) = top_line(session) {
        let desc_label = gtk::Label::new(Some(&desc_text));
        desc_label.add_css_class("status-desc");
        desc_label.set_halign(gtk::Align::Fill);
        desc_label.set_xalign(0.0);
        desc_label.set_hexpand(true);
        desc_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        desc_label.set_max_width_chars(1);
        content.append(&desc_label);
    }

    let state_text = state_label(session);
    let action_label = gtk::Label::new(Some(&state_text));
    action_label.add_css_class("action-line");
    action_label.add_css_class(session.status.css_class());
    action_label.set_halign(gtk::Align::Fill);
    action_label.set_xalign(0.0);
    action_label.set_hexpand(true);
    action_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    action_label.set_max_width_chars(1);
    content.append(&action_label);

    if let Some(ref pending) = session.pending_approval {
        let bar = build_choice_bar(pending.request_id.clone(), &pending.choices);
        content.append(&bar);
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

/// Test hook: does this session currently expect a widget approval click?
pub(crate) fn has_pending_approval(session: &Session) -> bool {
    session.pending_approval.is_some()
}

/// Button text for a given choice — just returns its label.
pub(crate) fn button_label(choice: &crate::session::ApprovalChoice) -> &str {
    &choice.label
}

/// CSS class name for a choice. `allow` + `Some(suggestion)` renders as the
/// softer `approval-scope` (session-rule), plain allow as green `approval-accept`,
/// deny as red `approval-deny`.
pub(crate) fn button_css_class(choice: &crate::session::ApprovalChoice) -> &'static str {
    match (choice.behavior.as_str(), choice.suggestion.is_some()) {
        ("allow", true) => "approval-scope",
        ("allow", false) => "approval-accept",
        ("deny", _) => "approval-deny",
        _ => "approval-accept",
    }
}

/// Build a vertical box containing one full-width button per ApprovalChoice.
/// Buttons stack so the card never demands more horizontal space than the
/// panel width, regardless of how long a suggestion label is.
/// Click handler sends `ApprovalDecision { request_id, choice_index }`.
fn build_choice_bar(
    request_id: String,
    choices: &[crate::session::ApprovalChoice],
) -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Vertical, 4);
    bar.add_css_class("approval-bar");
    bar.set_halign(gtk::Align::Fill);
    bar.set_hexpand(true);
    bar.set_margin_top(4);

    for (idx, choice) in choices.iter().enumerate() {
        let label = gtk::Label::new(Some(button_label(choice)));
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_max_width_chars(1);
        label.set_hexpand(true);
        label.set_xalign(0.5);

        let button = gtk::Button::new();
        button.set_child(Some(&label));
        button.add_css_class(button_css_class(choice));
        button.set_hexpand(true);
        button.set_halign(gtk::Align::Fill);

        let rid = request_id.clone();
        button.connect_clicked(move |_| {
            let rid = rid.clone();
            std::thread::spawn(move || {
                send_approval_decision(&rid, idx);
            });
        });
        bar.append(&button);
    }

    bar
}

/// Maximum characters of prompt/agent text to render before ellipsizing.
const DESCRIBE_MAX_CHARS: usize = 60;

/// First content line: whichever of "live tool call" / "last prompt" / "last
/// agent text" is most current. Returns `None` only when nothing has been
/// captured yet (fresh session, no activity).
///
/// While a tool is running (Executing/WaitingApproval with tool+detail), the
/// tool action wins — it's the freshest thing happening. Once the tool
/// finishes, the agent text posted by PostToolUse takes over.
pub(crate) fn top_line(session: &Session) -> Option<String> {
    if matches!(
        session.status,
        SessionStatus::Executing | SessionStatus::WaitingApproval
    ) {
        if let (Some(tool), Some(detail)) = (&session.current_tool, &session.tool_detail) {
            return Some(truncate(&describe_tool(tool, detail, true), DESCRIBE_MAX_CHARS));
        }
        if let Some(tool) = session.current_tool.as_deref() {
            // Tool is running but the hook payload had no `command`/`file_path`
            // (common for MCP and AskUserQuestion-style inputs). Show the
            // tool name anyway so line 1 still reflects what's happening now.
            return Some(format!("Running {}", prettify_tool_name(tool)));
        }
    }

    let user = session.last_prompt.as_deref().zip(session.last_prompt_at);
    let agent = session.last_agent_text.as_deref().zip(session.last_agent_text_at);
    match (user, agent) {
        (Some((p, _)), None) => Some(render_user(p)),
        (None, Some((a, _))) => Some(render_agent(session, a)),
        (Some((p, pu)), Some((a, au))) => Some(if pu > au {
            render_user(p)
        } else {
            render_agent(session, a)
        }),
        (None, None) => None,
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

/// Short state word for the second line — same vocabulary as the waybar
/// widget (`idle`, `thinking`, `Bash`, `Edit`, `approval`, `stopped`).
fn state_label(session: &Session) -> String {
    session.inline_status()
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

/// Send an `ApprovalDecision` event to the running daemon on its IPC socket.
/// Called from Accept/Deny button click handlers on a spawned OS thread.
fn send_approval_decision(request_id: &str, choice_index: usize) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("vibewatch: failed to build tokio rt for approval: {e}");
            return;
        }
    };
    let request_id = request_id.to_string();
    rt.block_on(async move {
        let config = match crate::config::Config::load() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("vibewatch: config load failed: {e}");
                return;
            }
        };
        let event = crate::ipc::InboundEvent::ApprovalDecision {
            request_id,
            choice_index,
        };
        if let Err(e) = crate::ipc::send_event(&config.socket_path(), &event).await {
            eprintln!("vibewatch: send_event ApprovalDecision failed: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{button_css_class, button_label, has_pending_approval, state_label, top_line};
    use crate::session::{AgentKind, Session, SessionStatus};

    fn mk(agent: AgentKind) -> Session {
        Session::new("s1".into(), agent, 1)
    }

    #[test]
    fn top_line_user_only_renders_you_prefix() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.last_prompt = Some("fix the deploy".into());
        s.last_prompt_at = Some(100);
        assert_eq!(top_line(&s).as_deref(), Some("You: \"fix the deploy\""));
    }

    #[test]
    fn top_line_agent_only_renders_agent_prefix() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.last_agent_text = Some("Tests pass.".into());
        s.last_agent_text_at = Some(100);
        assert_eq!(top_line(&s).as_deref(), Some("Claude: \"Tests pass.\""));
    }

    #[test]
    fn top_line_codex_agent_uses_codex_prefix() {
        let mut s = mk(AgentKind::Codex);
        s.last_agent_text = Some("All good.".into());
        s.last_agent_text_at = Some(100);
        assert_eq!(top_line(&s).as_deref(), Some("Codex: \"All good.\""));
    }

    #[test]
    fn top_line_user_wins_when_newer() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.last_prompt = Some("please do X".into());
        s.last_prompt_at = Some(200);
        s.last_agent_text = Some("done".into());
        s.last_agent_text_at = Some(100);
        assert_eq!(top_line(&s).as_deref(), Some("You: \"please do X\""));
    }

    #[test]
    fn top_line_agent_wins_when_newer_or_equal() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.last_prompt = Some("please do X".into());
        s.last_prompt_at = Some(100);
        s.last_agent_text = Some("done".into());
        s.last_agent_text_at = Some(100);
        assert_eq!(top_line(&s).as_deref(), Some("Claude: \"done\""));
    }

    #[test]
    fn top_line_none_when_nothing_captured() {
        let s = mk(AgentKind::ClaudeCode);
        assert!(top_line(&s).is_none());
    }

    #[test]
    fn top_line_none_when_stopped_with_no_content() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.status = SessionStatus::Stopped;
        assert!(top_line(&s).is_none());
    }

    #[test]
    fn top_line_live_tool_wins_while_executing() {
        // Executing with tool+detail overrides older prompt/agent text — the
        // running tool is always "freshest".
        let mut s = mk(AgentKind::ClaudeCode);
        s.status = SessionStatus::Executing;
        s.current_tool = Some("Edit".into());
        s.tool_detail = Some("src/main.rs".into());
        s.last_agent_text = Some("done".into());
        s.last_agent_text_at = Some(999);
        assert_eq!(top_line(&s).as_deref(), Some("Editing src/main.rs"));
    }

    #[test]
    fn top_line_running_fallback_when_no_detail() {
        // MCP tools and AskUserQuestion don't carry `command`/`file_path`, so
        // the hook sets `current_tool` but leaves `tool_detail` empty. We
        // still want line 1 to reflect what's happening.
        let mut s = mk(AgentKind::ClaudeCode);
        s.status = SessionStatus::Executing;
        s.current_tool = Some("mcp__claude_ai_Linear__list_issues".into());
        assert_eq!(top_line(&s).as_deref(), Some("Running Linear.list_issues"));
    }

    #[test]
    fn top_line_live_tool_wins_while_waiting_approval() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.status = SessionStatus::WaitingApproval;
        s.current_tool = Some("Bash".into());
        s.tool_detail = Some("rm -rf /tmp/foo".into());
        assert_eq!(top_line(&s).as_deref(), Some("rm -rf /tmp/foo"));
    }

    #[test]
    fn top_line_falls_back_to_agent_text_after_post_tool_use() {
        // PostToolUse clears current_tool and sets status to Thinking — the
        // last agent text becomes the freshest thing to show.
        let mut s = mk(AgentKind::ClaudeCode);
        s.status = SessionStatus::Thinking;
        s.last_tool = Some("Edit".into());
        s.last_agent_text = Some("Applied the change.".into());
        s.last_agent_text_at = Some(500);
        assert_eq!(
            top_line(&s).as_deref(),
            Some("Claude: \"Applied the change.\"")
        );
    }

    #[test]
    fn top_line_long_text_is_truncated_with_ellipsis() {
        let mut s = mk(AgentKind::ClaudeCode);
        let long: String = "x".repeat(200);
        s.last_prompt = Some(long.clone());
        s.last_prompt_at = Some(1);
        let out = top_line(&s).unwrap();
        assert!(out.starts_with("You: \""));
        assert!(out.ends_with("...\""));
        assert!(out.len() < long.len() + 20);
    }

    #[test]
    fn state_label_idle_by_default() {
        let s = mk(AgentKind::ClaudeCode);
        assert_eq!(state_label(&s), "idle");
    }

    #[test]
    fn state_label_thinking_when_thinking() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.status = SessionStatus::Thinking;
        assert_eq!(state_label(&s), "thinking");
    }

    #[test]
    fn state_label_exec_fallback_when_no_tool() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.status = SessionStatus::Executing;
        assert_eq!(state_label(&s), "exec");
    }

    #[test]
    fn state_label_shows_tool_name_when_executing() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.status = SessionStatus::Executing;
        s.current_tool = Some("Edit".into());
        assert_eq!(state_label(&s), "Edit");
    }

    #[test]
    fn state_label_stopped_when_stopped() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.status = SessionStatus::Stopped;
        assert_eq!(state_label(&s), "stopped");
    }

    #[test]
    fn state_label_approval_when_waiting() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.status = SessionStatus::WaitingApproval;
        s.current_tool = Some("Bash".into());
        assert_eq!(state_label(&s), "approval");
    }

    #[test]
    fn has_pending_approval_returns_true_when_set() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.pending_approval = Some(crate::session::PendingApproval {
            request_id: "r1".into(),
            tool: "Bash".into(),
            detail: Some("ls".into()),
            choices: vec![],
        });
        assert!(has_pending_approval(&s));
    }

    #[test]
    fn has_pending_approval_returns_false_when_none() {
        let s = mk(AgentKind::ClaudeCode);
        assert!(!has_pending_approval(&s));
    }

    #[test]
    fn button_label_for_plain_yes_is_yes() {
        let c = crate::session::ApprovalChoice {
            label: "Yes".into(),
            behavior: "allow".into(),
            suggestion: None,
        };
        assert_eq!(button_label(&c), "Yes");
    }

    #[test]
    fn button_css_class_for_suggestion_is_approval_scope() {
        let c = crate::session::ApprovalChoice {
            label: "Yes, allow Read for /foo (session)".into(),
            behavior: "allow".into(),
            suggestion: Some(crate::session::PermissionSuggestion {
                kind: "addRules".into(),
                rules: vec![],
                behavior: "allow".into(),
                destination: "session".into(),
            }),
        };
        assert_eq!(button_css_class(&c), "approval-scope");
    }

    #[test]
    fn button_css_class_plain_allow_is_accept() {
        let c = crate::session::ApprovalChoice {
            label: "Yes".into(),
            behavior: "allow".into(),
            suggestion: None,
        };
        assert_eq!(button_css_class(&c), "approval-accept");
    }

    #[test]
    fn button_css_class_deny_is_deny() {
        let c = crate::session::ApprovalChoice {
            label: "No".into(),
            behavior: "deny".into(),
            suggestion: None,
        };
        assert_eq!(button_css_class(&c), "approval-deny");
    }
}
