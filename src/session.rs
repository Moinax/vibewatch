use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, RwLock};

/// Claude Code tool names we special-case across the daemon, hook, and panel.
/// Centralised so a typo in one place doesn't silently break the special path.
pub const TOOL_EXIT_PLAN_MODE: &str = "ExitPlanMode";
pub const TOOL_ASK_USER_QUESTION: &str = "AskUserQuestion";

/// `/proc/<pid>/comm` values we accept as "this PID is still Claude Code".
/// Used by the scanner for discovery and by the registry for liveness checks,
/// so a rename here updates both paths in lockstep.
pub const CLAUDE_CODE_COMMS: &[&str] = &["claude"];

/// `/proc/<pid>/comm` values we accept as "this PID is still Codex".
pub const CODEX_COMMS: &[&str] = &["codex"];

/// Map an `AgentKind` to the `/proc/<pid>/comm` values that identify it.
/// Returns an empty slice for window-backed agents (Cursor, WebStorm) —
/// their liveness is tracked by the compositor scan, not by `/proc`.
pub fn expected_comms_for(kind: AgentKind) -> &'static [&'static str] {
    match kind {
        AgentKind::ClaudeCode => CLAUDE_CODE_COMMS,
        AgentKind::Codex => CODEX_COMMS,
        AgentKind::Cursor | AgentKind::WebStorm => &[],
    }
}

/// Everything we need to derive from a running agent's `/proc/<pid>/cmdline`
/// in a single read — whether it's a programmatic (non-interactive)
/// invocation, and the `--resume` / `--continue` / `-c` session name if any.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PidCmdlineInfo {
    /// True for third-party tools (T3 Chat, editors, automation) that drive
    /// `claude` as a stream-JSON subprocess — not tied to a terminal the
    /// user is interacting with, so the panel shouldn't track them.
    pub programmatic: bool,
    pub session_name: Option<String>,
}

pub fn inspect_pid_cmdline(pid: u32) -> PidCmdlineInfo {
    let Ok(raw) = std::fs::read_to_string(format!("/proc/{}/cmdline", pid)) else {
        return PidCmdlineInfo::default();
    };
    let args: Vec<&str> = raw.split('\0').collect();
    PidCmdlineInfo {
        programmatic: is_programmatic_args(&args),
        session_name: session_name_from_args(&args),
    }
}

fn is_programmatic_args(args: &[&str]) -> bool {
    if args.iter().any(|a| *a == "--no-session-persistence") {
        return true;
    }
    args.windows(2)
        .any(|w| w[0] == "--output-format" && w[1] == "stream-json")
}

fn session_name_from_args(args: &[&str]) -> Option<String> {
    args.windows(2).find_map(|w| {
        if matches!(w[0], "--resume" | "--continue" | "-c") {
            let name = w[1].trim();
            if !name.is_empty() && !name.starts_with('-') {
                return Some(name.to_string());
            }
        }
        None
    })
}

/// Kind of AI agent being monitored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// One entry of `decision.updatedPermissions` sent back to Claude Code — lets
/// the widget mirror the TUI's "and auto-accept edits for this session" option
/// by flipping the session's permission mode when the button is clicked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatedPermission {
    #[serde(rename = "type")]
    pub kind: String,
    pub mode: String,
    pub destination: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalChoice {
    pub label: String,
    pub behavior: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<PermissionSuggestion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_permissions: Option<Vec<UpdatedPermission>>,
}

impl ApprovalChoice {
    /// Build the ordered Yes / suggestions… / No button list. Returns empty
    /// for tools the panel can't faithfully answer (ExitPlanMode,
    /// AskUserQuestion) — those render as warning-only and the user
    /// answers in Claude Code's TUI.
    pub fn build_from(tool_name: &str, suggestions: &[PermissionSuggestion]) -> Vec<ApprovalChoice> {
        if tool_name == TOOL_EXIT_PLAN_MODE || tool_name == TOOL_ASK_USER_QUESTION {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(2 + suggestions.len());
        out.push(ApprovalChoice {
            label: "Yes".to_string(),
            behavior: "allow".to_string(),
            suggestion: None,
            updated_permissions: None,
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
                updated_permissions: None,
            });
        }
        out.push(ApprovalChoice {
            label: "No".to_string(),
            behavior: "deny".to_string(),
            suggestion: None,
            updated_permissions: None,
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
                updated_permissions: None,
            })
            .collect()
    }

    /// Panel button CSS class derived from `behavior` + whether a suggestion
    /// is attached. Drives the Catppuccin color story:
    /// allow + suggestion → lavender (session-scope rule), plain allow →
    /// green (accept), deny → red, answer → teal (AskUserQuestion option).
    pub fn css_class(&self) -> &'static str {
        match (self.behavior.as_str(), self.suggestion.is_some()) {
            ("allow", true) => "approval-scope",
            ("allow", false) => "approval-accept",
            ("deny", _) => "approval-deny",
            ("answer", _) => "approval-answer",
            _ => "approval-accept",
        }
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
    /// Unix epoch seconds when `last_tool` finished (PostToolUse). Used by
    /// the panel's line-1 picker so a completed tool stays visible as the
    /// most recent event until a newer prompt or agent text arrives.
    #[serde(default)]
    pub last_tool_at: Option<u64>,
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
            last_tool_at: None,
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
        if let Ok(path) = std::fs::read_link(format!("/proc/{}/cwd", self.pid)) {
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

    /// Store a new transcript line as `last_agent_text` and stamp
    /// `last_agent_text_at`, but only when `text` actually differs from what's
    /// already stored. Returns whether a mutation happened — callers use it
    /// to skip unnecessary re-registers and notify wakes.
    pub fn set_last_agent_text_if_changed(&mut self, text: String) -> bool {
        if self.last_agent_text.as_deref() == Some(text.as_str()) {
            return false;
        }
        self.last_agent_text = Some(text);
        self.last_agent_text_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs());
        true
    }

    /// Short inline status text for waybar/status display.
    pub fn inline_status(&self) -> String {
        match self.status {
            SessionStatus::Executing => self
                .current_tool
                .as_deref()
                .map(prettify_tool_name)
                .unwrap_or_else(|| "exec".to_string()),
            SessionStatus::WaitingApproval => {
                // AskUserQuestion waits for an answer (the user picks an
                // option), not an approval gate like Bash or ExitPlanMode.
                if self.current_tool.as_deref() == Some(TOOL_ASK_USER_QUESTION) {
                    "awaiting answer".to_string()
                } else {
                    "awaiting approval".to_string()
                }
            }
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
#[derive(Debug, Clone, Default)]
pub struct SessionRegistry {
    sessions: Arc<RwLock<HashMap<String, Session>>>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new session, replacing any previous session with the same id.
    pub fn register(&self, session: Session) {
        let mut map = self.sessions.write().unwrap();
        map.insert(session.id.clone(), session);
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

    /// Look up a session by id; if missing and any session exists for the
    /// same `pid`, rename it in-place to `new_id` and return the renamed
    /// session. Rehomes scanner sessions under real hook session ids, and
    /// also recovers when a parent session's id was superseded (e.g. by a
    /// sibling SessionStart on the same process).
    pub fn get_or_adopt(&self, new_id: &str, pid: u32) -> Option<Session> {
        let mut map = self.sessions.write().unwrap();
        if let Some(s) = map.get(new_id) {
            return Some(s.clone());
        }
        let adopt_id = map
            .iter()
            .find(|(_, s)| s.pid == pid)
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
        (_, _) => format!("{}: {}", prettify_tool_name(tool), detail),
    }
}

/// Prettify a raw Claude Code tool name for display.
///
/// MCP tool names arrive as `mcp__<server>__<tool>` and can be very long
/// (e.g. `mcp__claude_ai_Linear__list_issues`). We collapse the server
/// segment to its last underscore-token and join with a dot, giving
/// `Linear.list_issues`. Everything else is returned unchanged.
pub fn prettify_tool_name(name: &str) -> String {
    let Some(rest) = name.strip_prefix("mcp__") else {
        return name.to_string();
    };
    let Some((server, tool)) = rest.split_once("__") else {
        return name.to_string();
    };
    let server_short = server.rsplit('_').next().unwrap_or(server);
    format!("{}.{}", server_short, tool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programmatic_args_detect_no_session_persistence() {
        assert!(is_programmatic_args(&["claude", "--no-session-persistence"]));
    }

    #[test]
    fn programmatic_args_detect_stream_json_output() {
        assert!(is_programmatic_args(&["claude", "--output-format", "stream-json"]));
    }

    #[test]
    fn programmatic_args_ignore_interactive_text_output() {
        assert!(!is_programmatic_args(&["claude", "--output-format", "text"]));
    }

    #[test]
    fn programmatic_args_ignore_plain_interactive() {
        assert!(!is_programmatic_args(&["claude", "--resume", "work"]));
    }

    #[test]
    fn session_name_from_args_reads_resume() {
        assert_eq!(
            session_name_from_args(&["claude", "--resume", "my-session"]),
            Some("my-session".into()),
        );
    }

    #[test]
    fn session_name_from_args_skips_when_flag_value_is_another_flag() {
        assert_eq!(
            session_name_from_args(&["claude", "--continue", "--verbose"]),
            None,
        );
    }

    #[test]
    fn session_name_from_args_returns_none_without_resume() {
        assert_eq!(session_name_from_args(&["claude", "--verbose"]), None);
    }

    #[test]
    fn agent_display_name() {
        assert_eq!(AgentKind::ClaudeCode.display_name(), "Claude Code");
        assert_eq!(AgentKind::Codex.display_name(), "Codex");
        assert_eq!(AgentKind::Cursor.display_name(), "Cursor");
        assert_eq!(AgentKind::WebStorm.display_name(), "WebStorm");
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
    fn registry_remove() {
        let registry = SessionRegistry::new();
        registry.register(Session::new("s1".into(), AgentKind::Cursor, 42));
        assert!(registry.remove("s1").is_some());
        assert!(registry.get("s1").is_none());
    }

    #[test]
    fn prettify_mcp_name_shortens_to_server_dot_tool() {
        assert_eq!(
            prettify_tool_name("mcp__claude_ai_Linear__list_issues"),
            "Linear.list_issues"
        );
        assert_eq!(
            prettify_tool_name("mcp__plugin_context7_context7__query-docs"),
            "context7.query-docs"
        );
    }

    #[test]
    fn prettify_passes_through_non_mcp_names() {
        assert_eq!(prettify_tool_name("Bash"), "Bash");
        assert_eq!(prettify_tool_name("AskUserQuestion"), "AskUserQuestion");
        assert_eq!(prettify_tool_name(""), "");
    }

    #[test]
    fn prettify_handles_malformed_mcp_name() {
        // Missing second `__` separator — leave unchanged.
        assert_eq!(prettify_tool_name("mcp__weird_name"), "mcp__weird_name");
    }

    #[test]
    fn inline_status_prettifies_mcp_tool() {
        let mut s = Session::new("s".into(), AgentKind::ClaudeCode, 1);
        s.status = SessionStatus::Executing;
        s.current_tool = Some("mcp__claude_ai_Linear__list_issues".into());
        assert_eq!(s.inline_status(), "Linear.list_issues");
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
    fn registry_get_or_adopt_rehomes_uuid_session_by_pid() {
        // When a sibling SessionStart (subagent/Task) previously overwrote the
        // parent session's entry, a plain UUID session — not a scan- one —
        // is sitting in the registry with the parent PID. Hooks for the
        // original session must still be able to adopt it.
        let registry = SessionRegistry::new();
        let mut prior = Session::new("old-uuid-111".into(), AgentKind::ClaudeCode, 7777);
        prior.status = SessionStatus::Thinking;
        registry.register(prior);

        let adopted = registry
            .get_or_adopt("new-uuid-222", 7777)
            .expect("adopts sibling uuid session by pid");
        assert_eq!(adopted.id, "new-uuid-222");
        assert_eq!(adopted.status, SessionStatus::Thinking);
        assert!(registry.get("old-uuid-111").is_none());
        assert!(registry.get("new-uuid-222").is_some());
    }

    #[test]
    fn registry_get_or_adopt_returns_none_when_no_pid_match() {
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
            updated_permissions: None,
        };
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("suggestion"), "got {json}");
        assert!(!json.contains("updatedPermissions"), "got {json}");
        assert!(!json.contains("updated_permissions"), "got {json}");
        assert!(json.contains(r#""label":"Yes""#));
    }

    #[test]
    fn build_choices_always_has_yes_first_and_no_last() {
        let choices = ApprovalChoice::build_from("Read", &[]);
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].label, "Yes");
        assert_eq!(choices[0].behavior, "allow");
        assert!(choices[0].suggestion.is_none());
        assert!(choices[0].updated_permissions.is_none());
        assert_eq!(choices[1].label, "No");
        assert_eq!(choices[1].behavior, "deny");
    }

    #[test]
    fn build_choices_for_exit_plan_mode_returns_empty_so_panel_shows_warning_only() {
        // Claude Code's TUI renders ExitPlanMode options we can't see via
        // hooks; the panel shows the approval warning + clickable card and
        // the user answers in the TUI.
        let choices = ApprovalChoice::build_from("ExitPlanMode", &[]);
        assert!(choices.is_empty());
    }

    #[test]
    fn build_choices_for_exit_plan_mode_ignores_suggestions() {
        let sug = PermissionSuggestion {
            kind: "setMode".into(),
            rules: vec![],
            behavior: "allow".into(),
            destination: "session".into(),
        };
        let choices = ApprovalChoice::build_from("ExitPlanMode", &[sug]);
        assert!(choices.is_empty());
    }

    fn choice(label: &str, behavior: &str, suggestion: Option<PermissionSuggestion>) -> ApprovalChoice {
        ApprovalChoice {
            label: label.into(),
            behavior: behavior.into(),
            suggestion,
            updated_permissions: None,
        }
    }

    #[test]
    fn css_class_for_suggestion_is_approval_scope() {
        let sug = PermissionSuggestion {
            kind: "addRules".into(),
            rules: vec![],
            behavior: "allow".into(),
            destination: "session".into(),
        };
        assert_eq!(choice("Yes, allow Read", "allow", Some(sug)).css_class(), "approval-scope");
    }

    #[test]
    fn css_class_plain_allow_is_accept() {
        assert_eq!(choice("Yes", "allow", None).css_class(), "approval-accept");
    }

    #[test]
    fn css_class_deny_is_deny() {
        assert_eq!(choice("No", "deny", None).css_class(), "approval-deny");
    }

    #[test]
    fn css_class_answer_is_approval_answer() {
        assert_eq!(choice("Option A", "answer", None).css_class(), "approval-answer");
    }

    #[test]
    fn updated_permission_serializes_with_type_key() {
        let up = UpdatedPermission {
            kind: "setMode".into(),
            mode: "acceptEdits".into(),
            destination: "session".into(),
        };
        let json = serde_json::to_string(&up).unwrap();
        assert!(json.contains(r#""type":"setMode""#), "got {json}");
        assert!(!json.contains("\"kind\""));
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

    #[test]
    fn expected_comms_for_cli_agents() {
        assert_eq!(expected_comms_for(AgentKind::ClaudeCode), &["claude"]);
        assert_eq!(expected_comms_for(AgentKind::Codex), &["codex"]);
    }

    #[test]
    fn expected_comms_for_window_agents_is_empty() {
        assert!(expected_comms_for(AgentKind::Cursor).is_empty());
        assert!(expected_comms_for(AgentKind::WebStorm).is_empty());
    }
}
