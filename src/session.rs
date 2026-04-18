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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRule {
    pub tool_name: String,
    pub rule_content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionSuggestion {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub rules: Vec<PermissionRule>,
    pub behavior: String,
    pub destination: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalChoice {
    pub label: String,
    pub behavior: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<PermissionSuggestion>,
}

impl ApprovalChoice {
    /// Build the ordered list of buttons for a permission dialog.
    /// Always prepends "Yes" / appends "No"; each suggestion becomes a
    /// middle button with a human-readable label.
    pub fn build_from(tool_name: &str, suggestions: &[PermissionSuggestion]) -> Vec<ApprovalChoice> {
        let mut out = Vec::with_capacity(2 + suggestions.len());
        out.push(ApprovalChoice {
            label: "Yes".to_string(),
            behavior: "allow".to_string(),
            suggestion: None,
        });
        for sug in suggestions {
            let rules_label = sug
                .rules
                .iter()
                .map(|r| {
                    // Rule contents often come as "//path/**" (Claude convention);
                    // normalize to a single leading slash so the label reads naturally.
                    let trimmed = r.rule_content.trim_start_matches('/');
                    format!("/{}", trimmed)
                })
                .collect::<Vec<_>>()
                .join(" + ");
            let label = format!(
                "{} {} for {} ({})",
                if sug.behavior == "allow" { "Yes, allow" } else { "No, deny" },
                tool_name,
                rules_label,
                sug.destination,
            );
            out.push(ApprovalChoice {
                label,
                behavior: sug.behavior.clone(),
                suggestion: Some(sug.clone()),
            });
        }
        out.push(ApprovalChoice {
            label: "No".to_string(),
            behavior: "deny".to_string(),
            suggestion: None,
        });
        out
    }

    /// Build choices for AskUserQuestion: one button per option label, no
    /// Yes/No wrapping. `behavior` is `"answer"` (a sentinel — the daemon
    /// writes the label back and the hook plugs it into
    /// `hookSpecificOutput.updatedInput.answers`).
    pub fn from_labels(labels: &[String]) -> Vec<ApprovalChoice> {
        labels
            .iter()
            .map(|label| ApprovalChoice {
                label: label.clone(),
                behavior: "answer".to_string(),
                suggestion: None,
            })
            .collect()
    }
}

/// A pending tool-approval request from the agent, awaiting the user's
/// widget click. Serializable so it appears in `vibewatch status` output;
/// the held socket stream lives in `ApprovalRegistry`, not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApproval {
    pub request_id: String,
    pub tool: String,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub choices: Vec<ApprovalChoice>,
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
    /// Set while the session is waiting on a user Accept/Deny click in
    /// the widget. `None` at all other times.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_approval: Option<PendingApproval>,
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
            pending_approval: None,
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

    /// Look up a session by id; if missing and a scanner-discovered session
    /// exists for the same `pid`, rename it in-place to `new_id` and return
    /// the renamed session. This rehomes scanner sessions under real hook
    /// session ids when hooks start arriving (e.g. after a daemon restart
    /// while an agent session was already running).
    pub fn get_or_adopt(&self, new_id: &str, pid: u32) -> Option<Session> {
        let mut map = self.sessions.write().unwrap();
        if let Some(s) = map.get(new_id) {
            return Some(s.clone());
        }
        let adopt_id = map
            .iter()
            .find(|(id, s)| s.pid == pid && id.starts_with("scan-"))
            .map(|(id, _)| id.clone())?;
        let mut session = map.remove(&adopt_id)?;
        session.id = new_id.to_string();
        map.insert(new_id.to_string(), session.clone());
        Some(session)
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
    fn registry_get_or_adopt_rehomes_scan_session_by_pid() {
        let registry = SessionRegistry::new();
        let mut scan = Session::new("scan-claude-4242".into(), AgentKind::ClaudeCode, 4242);
        scan.terminal = Some("Kitty".into());
        scan.cwd = Some("/tmp/proj".into());
        registry.register(scan);

        let adopted = registry
            .get_or_adopt("real-uuid-abc", 4242)
            .expect("adopts scan session");
        assert_eq!(adopted.id, "real-uuid-abc");
        assert_eq!(adopted.pid, 4242);
        assert_eq!(adopted.terminal.as_deref(), Some("Kitty"));
        assert_eq!(adopted.cwd.as_deref(), Some("/tmp/proj"));
        assert!(registry.get("scan-claude-4242").is_none());
        assert!(registry.get("real-uuid-abc").is_some());
    }

    #[test]
    fn registry_get_or_adopt_returns_existing_session_unchanged() {
        let registry = SessionRegistry::new();
        registry.register(Session::new("real-uuid".into(), AgentKind::ClaudeCode, 999));
        let got = registry.get_or_adopt("real-uuid", 999).unwrap();
        assert_eq!(got.id, "real-uuid");
        assert_eq!(got.pid, 999);
    }

    #[test]
    fn registry_get_or_adopt_returns_none_when_no_scan_match() {
        let registry = SessionRegistry::new();
        registry.register(Session::new("scan-claude-1".into(), AgentKind::ClaudeCode, 1));
        // Different pid — should NOT adopt.
        assert!(registry.get_or_adopt("uuid", 9999).is_none());
    }

    #[test]
    fn new_session_has_null_agent_and_prompt_timestamps() {
        let s = Session::new("s1".into(), AgentKind::ClaudeCode, 42);
        assert!(s.last_agent_text.is_none());
        assert!(s.last_agent_text_at.is_none());
        assert!(s.last_prompt_at.is_none());
        assert!(s.transcript_path.is_none());
    }

    #[test]
    fn new_session_has_no_pending_approval() {
        let s = Session::new("s1".into(), AgentKind::ClaudeCode, 42);
        assert!(s.pending_approval.is_none());
    }

    #[test]
    fn session_serializes_pending_approval_when_set() {
        let mut s = Session::new("s1".into(), AgentKind::ClaudeCode, 42);
        s.pending_approval = Some(PendingApproval {
            request_id: "req-xyz".into(),
            tool: "Bash".into(),
            detail: Some("rm -rf /tmp/foo".into()),
            choices: vec![],
        });
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""pending_approval":"#));
        assert!(json.contains(r#""request_id":"req-xyz""#));
        assert!(json.contains(r#""tool":"Bash""#));
        assert!(json.contains(r#""detail":"rm -rf /tmp/foo""#));
    }

    #[test]
    fn session_omits_pending_approval_when_none() {
        let s = Session::new("s1".into(), AgentKind::ClaudeCode, 42);
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("pending_approval"));
    }

    #[test]
    fn pending_approval_has_choices_field_defaulting_empty() {
        let p = PendingApproval {
            request_id: "r1".into(),
            tool: "Bash".into(),
            detail: None,
            choices: vec![],
        };
        assert!(p.choices.is_empty());
    }

    #[test]
    fn permission_suggestion_serializes_with_type_rename() {
        let s = PermissionSuggestion {
            kind: "addRules".into(),
            rules: vec![PermissionRule {
                tool_name: "Read".into(),
                rule_content: "//home/**".into(),
            }],
            behavior: "allow".into(),
            destination: "session".into(),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""type":"addRules""#), "got {json}");
        assert!(json.contains(r#""behavior":"allow""#));
        assert!(json.contains(r#""destination":"session""#));
        assert!(json.contains(r#""toolName":"Read""#),
            "PermissionRule must serialize with camelCase to match Claude payload; got {json}");
        assert!(!json.contains("tool_name"));
    }

    #[test]
    fn approval_choice_omits_suggestion_when_none() {
        let c = ApprovalChoice {
            label: "Yes".into(),
            behavior: "allow".into(),
            suggestion: None,
        };
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("suggestion"), "got {json}");
        assert!(json.contains(r#""label":"Yes""#));
    }

    #[test]
    fn build_choices_always_has_yes_first_and_no_last() {
        let choices = ApprovalChoice::build_from("Read", &[]);
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].label, "Yes");
        assert_eq!(choices[0].behavior, "allow");
        assert!(choices[0].suggestion.is_none());
        assert_eq!(choices[1].label, "No");
        assert_eq!(choices[1].behavior, "deny");
    }

    #[test]
    fn build_choices_expands_session_suggestion_with_human_label() {
        let sug = PermissionSuggestion {
            kind: "addRules".into(),
            rules: vec![PermissionRule {
                tool_name: "Read".into(),
                rule_content: "//home/moinax/.claude/**".into(),
            }],
            behavior: "allow".into(),
            destination: "session".into(),
        };
        let choices = ApprovalChoice::build_from("Read", std::slice::from_ref(&sug));
        assert_eq!(choices.len(), 3);
        assert_eq!(choices[0].label, "Yes");
        assert!(choices[1].label.contains("Read"));
        assert!(choices[1].label.contains("/home/moinax/.claude/**"));
        assert!(choices[1].label.contains("session"));
        assert_eq!(choices[1].behavior, "allow");
        assert_eq!(choices[1].suggestion.as_ref().unwrap().destination, "session");
        assert_eq!(choices[2].label, "No");
    }

    #[test]
    fn build_choices_multiple_rules_joined_with_plus() {
        let sug = PermissionSuggestion {
            kind: "addRules".into(),
            rules: vec![
                PermissionRule { tool_name: "Read".into(), rule_content: "//a/**".into() },
                PermissionRule { tool_name: "Read".into(), rule_content: "//b/**".into() },
            ],
            behavior: "allow".into(),
            destination: "session".into(),
        };
        let choices = ApprovalChoice::build_from("Read", std::slice::from_ref(&sug));
        assert!(choices[1].label.contains("/a/**"));
        assert!(choices[1].label.contains("/b/**"));
        assert!(choices[1].label.contains("+"));
    }
}
