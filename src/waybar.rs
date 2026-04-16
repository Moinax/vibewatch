use crate::ipc::StatusResponse;
use crate::session::{Session, SessionStatus};

/// Build a StatusResponse from a list of sessions.
pub fn build_status(sessions: &[Session]) -> StatusResponse {
    let active: Vec<&Session> = sessions
        .iter()
        .filter(|s| s.status != SessionStatus::Stopped)
        .collect();

    let count = active.len();

    let text = if count > 0 {
        format!("\u{f544} {count}")
    } else {
        "\u{f544}".to_string()
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
        assert_eq!(status.text, "\u{f544}");
        assert_eq!(status.tooltip, "No agents running");
        assert_eq!(status.class, "idle");
    }

    #[test]
    fn test_active_agents() {
        let sessions = vec![
            make_session("s1", AgentKind::ClaudeCode, SessionStatus::Thinking),
            make_session("s2", AgentKind::Codex, SessionStatus::Executing),
        ];
        let status = build_status(&sessions);
        assert_eq!(status.text, "\u{f544} 2");
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
    }

    #[test]
    fn test_stopped_sessions_excluded_from_count() {
        let sessions = vec![
            make_session("s1", AgentKind::ClaudeCode, SessionStatus::Thinking),
            make_session("s2", AgentKind::Codex, SessionStatus::Stopped),
        ];
        let status = build_status(&sessions);
        assert_eq!(status.text, "\u{f544} 1");
    }

    #[test]
    fn test_status_with_tool_detail() {
        let mut session = make_session("s1", AgentKind::ClaudeCode, SessionStatus::Executing);
        session.current_tool = Some("Bash".to_string());
        session.tool_detail = Some("npm test".to_string());
        let status = build_status(&[session]);
        assert!(status.tooltip.contains("executing Bash: npm test")
            || status.tooltip.contains("Bash: npm test"));
    }
}
