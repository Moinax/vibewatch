use crate::ipc::StatusResponse;
use crate::session::{Session, SessionStatus};

/// Build a StatusResponse from a list of sessions.
pub fn build_status(sessions: &[Session]) -> StatusResponse {
    let active: Vec<&Session> = sessions
        .iter()
        .filter(|s| s.status != SessionStatus::Stopped)
        .collect();

    let count = active.len();

    let text = match count {
        0 => "AI".to_string(),
        1 => {
            let s = active[0];
            format!("AI {}: {}", s.agent.short_name(), s.inline_status())
        }
        _ => {
            // Pick the most interesting session
            let most_interesting = active
                .iter()
                .max_by_key(|s| s.interest_priority())
                .unwrap();
            format!(
                "AI {} \u{00b7} {}: {}",
                count,
                most_interesting.agent.short_name(),
                most_interesting.inline_status()
            )
        }
    };

    let tooltip = if active.is_empty() {
        "No agents running".to_string()
    } else {
        active
            .iter()
            .map(|s| s.status_line())
            .collect::<Vec<_>>()
            .join("\n")
    };

    let class = if sessions.iter().any(|s| s.status == SessionStatus::WaitingApproval) {
        "attention".to_string()
    } else if !active.is_empty() {
        "active".to_string()
    } else {
        "idle".to_string()
    };

    StatusResponse {
        text,
        tooltip,
        class,
        sessions: sessions.to_vec(),
    }
}

/// Print Waybar JSON to stdout (just text/tooltip/class, no sessions field).
pub fn print_waybar_status(sessions: &[Session]) {
    let status = build_status(sessions);
    let waybar_json = serde_json::json!({
        "text": status.text,
        "tooltip": status.tooltip,
        "class": status.class,
    });
    println!("{}", waybar_json);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{AgentKind, SessionStatus};

    fn make_session(id: &str, agent: AgentKind, status: SessionStatus) -> Session {
        let mut s = Session::new(id.to_string(), agent, 1000);
        s.status = status;
        s
    }

    #[test]
    fn test_empty_status() {
        let status = build_status(&[]);
        assert_eq!(status.text, "AI");
        assert_eq!(status.tooltip, "No agents running");
        assert_eq!(status.class, "idle");
    }

    #[test]
    fn test_single_agent() {
        let sessions = vec![make_session(
            "s1",
            AgentKind::ClaudeCode,
            SessionStatus::Thinking,
        )];
        let status = build_status(&sessions);
        assert_eq!(status.text, "AI Claude: thinking");
        assert_eq!(status.class, "active");
    }

    #[test]
    fn test_active_agents() {
        let sessions = vec![
            make_session("s1", AgentKind::ClaudeCode, SessionStatus::Thinking),
            make_session("s2", AgentKind::Codex, SessionStatus::Executing),
        ];
        let status = build_status(&sessions);
        // Executing has higher priority than Thinking, so Codex should be shown
        assert_eq!(status.text, "AI 2 \u{00b7} Codex: exec");
        assert_eq!(status.class, "active");
        assert!(status.tooltip.contains("Claude Code"));
        assert!(status.tooltip.contains("Codex"));
    }

    #[test]
    fn test_attention_class_when_waiting_approval() {
        let sessions = vec![make_session(
            "s1",
            AgentKind::ClaudeCode,
            SessionStatus::WaitingApproval,
        )];
        let status = build_status(&sessions);
        assert_eq!(status.class, "attention");
        assert_eq!(status.text, "AI Claude: approval");
    }

    #[test]
    fn test_stopped_sessions_excluded_from_count() {
        let sessions = vec![
            make_session("s1", AgentKind::ClaudeCode, SessionStatus::Thinking),
            make_session("s2", AgentKind::Codex, SessionStatus::Stopped),
        ];
        let status = build_status(&sessions);
        assert_eq!(status.text, "AI Claude: thinking");
    }

    #[test]
    fn test_status_with_tool_detail() {
        let mut session = make_session("s1", AgentKind::ClaudeCode, SessionStatus::Executing);
        session.current_tool = Some("Bash".to_string());
        session.tool_detail = Some("npm test".to_string());
        let status = build_status(&[session]);
        assert_eq!(status.text, "AI Claude: Bash");
        assert!(status.tooltip.contains("Claude Code: Bash"));
    }
}
