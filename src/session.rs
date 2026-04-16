use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Instant;

/// Kind of AI agent being monitored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    ClaudeCode,
    Codex,
    Cursor,
    WebStorm,
}

impl AgentKind {
    pub fn display_name(&self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "Claude Code",
            AgentKind::Codex => "Codex",
            AgentKind::Cursor => "Cursor",
            AgentKind::WebStorm => "WebStorm",
        }
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

/// Current status of an agent session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Thinking,
    Executing,
    WaitingApproval,
    Idle,
    Running,
    Stopped,
}

impl SessionStatus {
    pub fn css_class(&self) -> &'static str {
        match self {
            SessionStatus::Thinking => "thinking",
            SessionStatus::Executing => "executing",
            SessionStatus::WaitingApproval => "waiting-approval",
            SessionStatus::Idle => "idle",
            SessionStatus::Running => "running",
            SessionStatus::Stopped => "stopped",
        }
    }
}

/// A single monitored agent session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub agent: AgentKind,
    pub status: SessionStatus,
    pub current_tool: Option<String>,
    pub tool_detail: Option<String>,
    pub window_id: Option<String>,
    pub pid: u32,
    #[serde(skip)]
    pub started_at: Option<Instant>,
    #[serde(skip)]
    pub last_event: Option<Instant>,
}

impl Session {
    pub fn new(id: String, agent: AgentKind, pid: u32) -> Self {
        Self {
            id,
            agent,
            status: SessionStatus::Running,
            current_tool: None,
            tool_detail: None,
            window_id: None,
            pid,
            started_at: Some(Instant::now()),
            last_event: Some(Instant::now()),
        }
    }

    /// Update the last_event timestamp to now.
    pub fn touch(&mut self) {
        self.last_event = Some(Instant::now());
    }

    /// Human-readable one-line status.
    pub fn status_line(&self) -> String {
        let tool_info = match (&self.current_tool, &self.tool_detail) {
            (Some(tool), Some(detail)) => format!(" [{tool}: {detail}]"),
            (Some(tool), None) => format!(" [{tool}]"),
            _ => String::new(),
        };
        format!(
            "{} ({}): {:?}{}",
            self.agent.display_name(),
            self.id,
            self.status,
            tool_info,
        )
    }
}

/// Thread-safe registry of active sessions.
#[derive(Debug, Clone)]
pub struct SessionRegistry {
    sessions: Arc<RwLock<HashMap<String, Session>>>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a new session, replacing any previous session with the same id.
    pub fn register(&self, session: Session) {
        let mut map = self.sessions.write().unwrap();
        map.insert(session.id.clone(), session);
    }

    /// Update the status of an existing session. Returns false if the session
    /// does not exist.
    pub fn update_status(&self, id: &str, status: SessionStatus) -> bool {
        let mut map = self.sessions.write().unwrap();
        if let Some(session) = map.get_mut(id) {
            session.status = status;
            session.touch();
            true
        } else {
            false
        }
    }

    /// Remove a session by id. Returns the removed session if it existed.
    pub fn remove(&self, id: &str) -> Option<Session> {
        let mut map = self.sessions.write().unwrap();
        map.remove(id)
    }

    /// Get a clone of a session by id.
    pub fn get(&self, id: &str) -> Option<Session> {
        let map = self.sessions.read().unwrap();
        map.get(id).cloned()
    }

    /// Get clones of all sessions.
    pub fn all(&self) -> Vec<Session> {
        let map = self.sessions.read().unwrap();
        map.values().cloned().collect()
    }

    /// Count sessions that are not Stopped.
    pub fn active_count(&self) -> usize {
        let map = self.sessions.read().unwrap();
        map.values()
            .filter(|s| s.status != SessionStatus::Stopped)
            .count()
    }

    /// Remove sessions whose PID is no longer alive.
    pub fn cleanup_dead(&self) {
        let mut map = self.sessions.write().unwrap();
        map.retain(|_, session| is_pid_alive(session.pid));
    }

    /// Set the window id for a session. Returns false if the session does not exist.
    pub fn set_window_id(&self, id: &str, window_id: String) -> bool {
        let mut map = self.sessions.write().unwrap();
        if let Some(session) = map.get_mut(id) {
            session.window_id = Some(window_id);
            true
        } else {
            false
        }
    }
}

/// Check whether a process with the given PID is alive by probing /proc.
pub fn is_pid_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{}", pid)).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_display_name() {
        assert_eq!(AgentKind::ClaudeCode.display_name(), "Claude Code");
        assert_eq!(AgentKind::Codex.display_name(), "Codex");
        assert_eq!(AgentKind::Cursor.display_name(), "Cursor");
        assert_eq!(AgentKind::WebStorm.display_name(), "WebStorm");
    }

    #[test]
    fn session_status_line() {
        let mut session = Session::new("abc123".into(), AgentKind::ClaudeCode, 1234);
        session.status = SessionStatus::Thinking;
        let line = session.status_line();
        assert!(line.contains("Claude Code"));
        assert!(line.contains("abc123"));
        assert!(line.contains("Thinking"));

        // With tool info
        session.current_tool = Some("Read".into());
        session.tool_detail = Some("src/main.rs".into());
        let line = session.status_line();
        assert!(line.contains("[Read: src/main.rs]"));
    }

    #[test]
    fn session_status_css_class() {
        assert_eq!(SessionStatus::Thinking.css_class(), "thinking");
        assert_eq!(SessionStatus::Executing.css_class(), "executing");
        assert_eq!(
            SessionStatus::WaitingApproval.css_class(),
            "waiting-approval"
        );
        assert_eq!(SessionStatus::Idle.css_class(), "idle");
        assert_eq!(SessionStatus::Running.css_class(), "running");
        assert_eq!(SessionStatus::Stopped.css_class(), "stopped");
    }

    #[test]
    fn registry_register_and_get() {
        let registry = SessionRegistry::new();
        let session = Session::new("s1".into(), AgentKind::Codex, 9999);
        registry.register(session);

        let retrieved = registry.get("s1").unwrap();
        assert_eq!(retrieved.agent, AgentKind::Codex);
        assert_eq!(retrieved.pid, 9999);
    }

    #[test]
    fn registry_update_status() {
        let registry = SessionRegistry::new();
        registry.register(Session::new("s1".into(), AgentKind::ClaudeCode, 1));

        assert!(registry.update_status("s1", SessionStatus::Thinking));
        let s = registry.get("s1").unwrap();
        assert_eq!(s.status, SessionStatus::Thinking);
    }

    #[test]
    fn registry_update_nonexistent_returns_false() {
        let registry = SessionRegistry::new();
        assert!(!registry.update_status("nope", SessionStatus::Idle));
    }

    #[test]
    fn registry_remove() {
        let registry = SessionRegistry::new();
        registry.register(Session::new("s1".into(), AgentKind::Cursor, 42));
        assert!(registry.remove("s1").is_some());
        assert!(registry.get("s1").is_none());
    }

    #[test]
    fn registry_active_count() {
        let registry = SessionRegistry::new();
        registry.register(Session::new("s1".into(), AgentKind::ClaudeCode, 1));
        registry.register(Session::new("s2".into(), AgentKind::Codex, 2));
        assert_eq!(registry.active_count(), 2);

        registry.update_status("s1", SessionStatus::Stopped);
        assert_eq!(registry.active_count(), 1);
    }

    #[test]
    fn is_pid_alive_test() {
        // PID 1 (init/systemd) should always be alive on Linux
        assert!(is_pid_alive(1));
        // A very high PID is almost certainly not alive
        assert!(!is_pid_alive(4_000_000));
    }
}
