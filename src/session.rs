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

    pub fn short_name(&self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "Claude",
            AgentKind::Codex => "Codex",
            AgentKind::Cursor => "Cursor",
            AgentKind::WebStorm => "WS",
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
    pub last_tool: Option<String>,
    pub last_tool_detail: Option<String>,
    pub last_prompt: Option<String>,
    pub session_name: Option<String>,
    pub window_id: Option<String>,
    pub cwd: Option<String>,
    pub terminal: Option<String>,
    pub pid: u32,
    /// Unix epoch seconds when session was first seen
    pub started_at_epoch: Option<u64>,
    /// Last assistant text line read from the transcript (Claude/Codex only).
    #[serde(default)]
    pub last_agent_text: Option<String>,
    /// Unix epoch seconds when `last_agent_text` was last updated.
    #[serde(default)]
    pub last_agent_text_at: Option<u64>,
    /// Unix epoch seconds when `last_prompt` was last set.
    #[serde(default)]
    pub last_prompt_at: Option<u64>,
    /// Cached path to the transcript file once resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<std::path::PathBuf>,
}

impl Session {
    pub fn new(id: String, agent: AgentKind, pid: u32) -> Self {
        Self {
            id,
            agent,
            status: SessionStatus::Idle,
            current_tool: None,
            tool_detail: None,
            last_tool: None,
            last_tool_detail: None,
            last_prompt: None,
            session_name: None,
            window_id: None,
            cwd: None,
            terminal: None,
            pid,
            started_at_epoch: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs()),
            last_agent_text: None,
            last_agent_text_at: None,
            last_prompt_at: None,
            transcript_path: None,
        }
    }

    /// Human-readable name: session name > project folder > agent name.
    pub fn display_name(&self) -> String {
        // Prefer session name (from /rename or auto-topic)
        if let Some(ref name) = self.session_name {
            return name.clone();
        }
        // Fall back to project folder
        if let Some(ref cwd) = self.cwd {
            let folder = std::path::Path::new(cwd)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(cwd);
            return folder.to_string();
        }
        // Try /proc for scanned sessions
        if let Some(path) = std::fs::read_link(format!("/proc/{}/cwd", self.pid)).ok() {
            if let Some(folder) = path.file_name().and_then(|n| n.to_str()) {
                return folder.to_string();
            }
        }
        self.agent.display_name().to_string()
    }

    /// Update the last-seen timestamp.
    pub fn touch(&mut self) {
        // No-op for now — started_at_epoch is set once at creation
    }

    /// Human-readable one-line status.
    pub fn status_line(&self) -> String {
        let status_text = self.inline_status();
        format!("{}: {}", self.agent.display_name(), status_text)
    }

    /// Short inline status text for waybar/status display.
    pub fn inline_status(&self) -> String {
        match self.status {
            SessionStatus::Executing => {
                if let Some(tool) = &self.current_tool {
                    tool.clone()
                } else {
                    "exec".to_string()
                }
            }
            SessionStatus::WaitingApproval => "approval".to_string(),
            SessionStatus::Thinking => "thinking".to_string(),
            SessionStatus::Running => "idle".to_string(),
            SessionStatus::Idle => "idle".to_string(),
            SessionStatus::Stopped => "stopped".to_string(),
        }
    }

    /// Priority for determining "most interesting" status (higher = more interesting).
    pub fn interest_priority(&self) -> u8 {
        match self.status {
            SessionStatus::Executing => 5,
            SessionStatus::WaitingApproval => 4,
            SessionStatus::Thinking => 3,
            SessionStatus::Running => 2,
            SessionStatus::Idle => 1,
            SessionStatus::Stopped => 0,
        }
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

    /// Remove any session matching the given PID (used to deduplicate scanner vs hook sessions).
    pub fn remove_by_pid(&self, pid: u32) {
        let mut map = self.sessions.write().unwrap();
        map.retain(|_, s| s.pid != pid);
    }

    /// Update the session name. Returns false if the session does not exist.
    pub fn set_session_name(&self, id: &str, name: String) -> bool {
        let mut map = self.sessions.write().unwrap();
        if let Some(session) = map.get_mut(id) {
            session.session_name = Some(name);
            true
        } else {
            false
        }
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

/// Read the session name from a Claude Code transcript (last custom-title entry).
pub fn read_transcript_name(session_id: &str) -> Option<String> {
    let claude_projects = dirs::home_dir()?.join(".claude/projects");
    for project in std::fs::read_dir(&claude_projects).ok()?.flatten() {
        let transcript = project.path().join(format!("{}.jsonl", session_id));
        if transcript.exists() {
            let content = std::fs::read_to_string(&transcript).ok()?;
            for line in content.lines().rev() {
                if line.contains("\"custom-title\"") {
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                        if let Some(title) = val.get("customTitle").and_then(|v| v.as_str()) {
                            return Some(title.to_string());
                        }
                    }
                }
            }
            return None;
        }
    }
    None
}

/// Get the parent PID by parsing /proc/{pid}/stat.
pub fn parent_pid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
    let rest = &stat[stat.rfind(')')? + 2..];
    let ppid: u32 = rest.split_whitespace().nth(1)?.parse().ok()?;
    (ppid > 1).then_some(ppid)
}

/// Detect which terminal hosts a process by walking up the process tree.
pub fn detect_terminal(pid: u32) -> String {
    let mut current = pid;
    for _ in 0..10 {
        if let Ok(comm) = std::fs::read_to_string(format!("/proc/{}/comm", current)) {
            match comm.trim() {
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
        match parent_pid(current) {
            Some(ppid) => current = ppid,
            None => break,
        }
    }
    "Term".to_string()
}

/// Format a tool action with the given verb form.
pub fn describe_tool(tool: &str, detail: &str, present: bool) -> String {
    match (tool, present) {
        ("Write", true) => format!("Writing {}", detail),
        ("Write", false) => format!("Wrote {}", detail),
        ("Edit", true) => format!("Editing {}", detail),
        ("Edit", false) => format!("Edited {}", detail),
        ("Read", true) => format!("Reading {}", detail),
        ("Read", false) => format!("Read {}", detail),
        ("Bash", _) => detail.to_string(),
        ("Grep" | "Glob", true) => format!("Searching {}", detail),
        ("Grep" | "Glob", false) => format!("Searched {}", detail),
        (_, _) => format!("{}: {}", tool, detail),
    }
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
        assert_eq!(session.status_line(), "Claude Code: thinking");

        session.status = SessionStatus::Executing;
        session.current_tool = Some("Bash".into());
        assert_eq!(session.status_line(), "Claude Code: Bash");

        session.current_tool = None;
        assert_eq!(session.status_line(), "Claude Code: exec");

        session.status = SessionStatus::Idle;
        assert_eq!(session.status_line(), "Claude Code: idle");
    }

    #[test]
    fn agent_short_name() {
        assert_eq!(AgentKind::ClaudeCode.short_name(), "Claude");
        assert_eq!(AgentKind::Codex.short_name(), "Codex");
        assert_eq!(AgentKind::Cursor.short_name(), "Cursor");
        assert_eq!(AgentKind::WebStorm.short_name(), "WS");
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

    #[test]
    fn new_session_has_null_agent_and_prompt_timestamps() {
        let s = Session::new("s1".into(), AgentKind::ClaudeCode, 42);
        assert!(s.last_agent_text.is_none());
        assert!(s.last_agent_text_at.is_none());
        assert!(s.last_prompt_at.is_none());
        assert!(s.transcript_path.is_none());
    }
}
