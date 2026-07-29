use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, RwLock};

/// Claude Code tool names we special-case across the daemon, hook, and panel.
/// Centralised so a typo in one place doesn't silently break the special path.
pub const TOOL_EXIT_PLAN_MODE: &str = "ExitPlanMode";
pub const TOOL_ASK_USER_QUESTION: &str = "AskUserQuestion";
/// Spawning a sub-agent, whose completion arrives later as its own `Stop`. Both
/// names are matched: `Task` is the wire name, `Agent` the current tool name, and
/// a rename must not silently stop the counting and bring spurious chimes back.
pub const TOOL_AGENT: &str = "Agent";
pub const TOOL_TASK: &str = "Task";

/// True for a tool that spawns a sub-agent.
pub fn spawns_subagent(tool: &str) -> bool {
    tool == TOOL_AGENT || tool == TOOL_TASK
}

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

/// Normalise a raw `/proc/<pid>/comm` read for comparison against our
/// comm constants: strip the trailing `\n` plus any surrounding whitespace,
/// and lowercase so matching is case-insensitive.
pub fn normalize_comm(comm: &str) -> String {
    comm.trim().to_lowercase()
}

/// Pure helper: does a `comm` string identify the given `AgentKind`?
pub fn is_agent_pid_alive_with_comm(comm: &str, kind: AgentKind) -> bool {
    let comm = normalize_comm(comm);
    expected_comms_for(kind).iter().any(|expected| comm == *expected)
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

    /// True when liveness is tracked by the compositor scan rather than by
    /// `/proc/<pid>/comm`. Window-backed agents' PIDs belong to GUI apps
    /// whose comm isn't in our agent-comm list, so `cleanup_dead` must
    /// exempt them and let the compositor scan in `scanner.rs` reap them.
    pub fn is_window_backed(&self) -> bool {
        matches!(self, AgentKind::Cursor | AgentKind::WebStorm)
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
    /// Unix epoch seconds of the last `Stop` — the moment the agent finished
    /// its turn — for as long as the user hasn't acknowledged it. Drives
    /// [`Session::just_finished`]; cleared by [`Session::acknowledge`] when
    /// the card is clicked.
    #[serde(default)]
    pub finished_at: Option<u64>,
    /// Bumped on every [`Session::mark_finished`]. The idle chime is announced
    /// on a delay, and this is how that delayed task tells "still the turn I was
    /// scheduled for" from "this session has finished again since". `finished_at`
    /// cannot answer that: it is in whole seconds, so two turns inside the
    /// debounce window carry the same stamp. Daemon bookkeeping, never the wire.
    #[serde(skip)]
    pub finish_seq: u64,
    /// Sub-agents this session has launched that have not reported back. While
    /// this is non-zero the turn is not over however quiet it looks: the main
    /// agent really did stop, but only to wait on background work that will
    /// re-invoke it. Reset on every user prompt, so a sub-agent that dies
    /// without a `Stop` cannot wedge the count for the life of the session.
    /// Daemon bookkeeping, never the wire.
    #[serde(skip)]
    pub pending_agents: u32,
}

/// Unix epoch seconds; 0 if the system clock predates the epoch.
pub fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
            started_at_epoch: Some(now_epoch()),
            last_agent_text: None,
            last_agent_text_at: None,
            last_prompt_at: None,
            transcript_path: None,
            pending_approval: None,
            finished_at: None,
            finish_seq: 0,
            pending_agents: 0,
        }
    }

    /// Stamp the moment the agent finished its turn. Called on `Stop`, right
    /// before the status flips to `Idle`.
    pub fn mark_finished(&mut self) {
        self.finished_at = Some(now_epoch());
        self.finish_seq = self.finish_seq.wrapping_add(1);
    }

    /// The agent launched a sub-agent, so its next stop is a wait, not a finish.
    pub fn launch_subagent(&mut self) {
        self.pending_agents += 1;
    }

    /// One sub-agent reported in. True when it was the last one outstanding,
    /// which is the moment a held turn becomes announceable again.
    ///
    /// Saturating, and false when the count was already zero: the launch may
    /// predate this daemon, or [`Session::reset_subagents`] may have cleared the
    /// count while the sub-agent was still running. Without that guard a late
    /// report would schedule a second announcement for a turn already announced.
    pub fn subagent_finished(&mut self) -> bool {
        if self.pending_agents == 0 {
            return false;
        }
        self.pending_agents -= 1;
        self.pending_agents == 0
    }

    /// Forget any outstanding sub-agents, returning how many were dropped.
    ///
    /// The count's safety net, called on each user prompt: a sub-agent that dies
    /// without reporting — a terminal API error, a killed pane — would otherwise
    /// leave the session waiting, and silent, for the rest of its life.
    pub fn reset_subagents(&mut self) -> u32 {
        std::mem::take(&mut self.pending_agents)
    }

    /// True when this session is a candidate to have its finish announced: it
    /// stopped, it has not picked the work back up, and nothing it launched is
    /// still running. Deliberately says nothing about *when* it stopped — that is
    /// the debounce's job, and [`Session::finish_seq`] identifies the turn.
    pub fn announceable(&self) -> bool {
        self.status == SessionStatus::Idle && self.finished_at.is_some() && self.pending_agents == 0
    }

    /// The user clicked this session's card: they are heading for the pane,
    /// so the row goes back to a plain idle one. Only the finished mark is
    /// dropped — a pending approval still needs a real answer.
    pub fn acknowledge(&mut self) {
        self.finished_at = None;
    }

    /// True from the moment the agent finishes its turn until the user
    /// clicks the card — or until the agent picks the work back up, since
    /// the state is gated on it still being idle. The panel lights these
    /// rows, ranks them near the top, and stays open while any exists, so
    /// the agent that chimed can't scroll past unnoticed.
    pub fn just_finished(&self) -> bool {
        self.status == SessionStatus::Idle && self.finished_at.is_some()
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
        self.last_agent_text_at = Some(now_epoch());
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

    /// Which band the session falls into when the panel groups its list:
    /// 0 = blocked on the user, 1 = just finished, 2 = working, 3 = idle,
    /// 4 = gone. `Running` is the scanner's "alive, but no hook data yet"
    /// state — it reads as `idle` everywhere else in the UI, so it bands with
    /// `Idle`. Band 1 is transient: a freshly finished agent sits there until
    /// the click acknowledging it drops it back into the idle band.
    pub fn activity_band(&self) -> u8 {
        match self.status {
            SessionStatus::WaitingApproval => 0,
            _ if self.just_finished() => 1,
            SessionStatus::Executing | SessionStatus::Thinking => 2,
            SessionStatus::Idle | SessionStatus::Running => 3,
            SessionStatus::Stopped => 4,
        }
    }

    /// Unix epoch seconds of the most recent thing that happened in this
    /// session — a tool, a prompt, an agent sentence, or failing all of those
    /// the moment we first saw it.
    pub fn last_activity_at(&self) -> u64 {
        [
            self.last_tool_at,
            self.last_prompt_at,
            self.last_agent_text_at,
            self.started_at_epoch,
        ]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(0)
    }
}

/// Order sessions the way the panel lists them: agents blocked on the user
/// first, then the ones that just finished, then the ones working, then idle,
/// then stopped.
///
/// The blocked and working bands sort by *start* time, not by last activity:
/// a working agent's activity timestamp moves every second, so ordering on it
/// would reshuffle the top of the list continuously — and rows are click
/// targets that focus a pane, so a row moving under the cursor is a misclick.
/// Start time never changes, which keeps a busy fleet still. Just-finished
/// agents go freshest-first — that band exists precisely to surface the one
/// that chimed last. Idle and stopped agents go most-recently-active first,
/// the only thing that separates them.
pub fn sort_by_activity(sessions: &mut [Session]) {
    fn key(s: &Session) -> (u8, std::cmp::Reverse<u64>) {
        let band = s.activity_band();
        let recency = match band {
            0 | 2 => s.started_at_epoch.unwrap_or(0),
            1 => s.finished_at.unwrap_or(0),
            _ => s.last_activity_at(),
        };
        (band, std::cmp::Reverse(recency))
    }
    // `id` is the final tiebreak so same-timestamp rows can't swap places
    // between two ticks of the panel's 10 Hz refresh.
    sessions.sort_by(|a, b| key(a).cmp(&key(b)).then_with(|| a.id.cmp(&b.id)));
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

    /// Like [`Self::all`], ordered for display by [`sort_by_activity`]. The
    /// panel uses this: it also makes the snapshot deterministic, which the
    /// panel's change-detection fingerprint depends on (`HashMap` iteration
    /// order shifts on rehash and would fake a change every few ticks).
    pub fn all_by_activity(&self) -> Vec<Session> {
        let mut sessions = self.all();
        sort_by_activity(&mut sessions);
        sessions
    }

    /// Remove sessions whose PID no longer hosts a process of the expected
    /// agent kind. Window-backed agents are exempted — the compositor scan
    /// in `scanner.rs` is authoritative for their liveness.
    pub fn cleanup_dead(&self) {
        let mut map = self.sessions.write().unwrap();
        map.retain(|_, session| {
            session.agent.is_window_backed() || is_agent_pid_alive(session.pid, session.agent)
        });
    }

    /// Enforce the invariant the rest of the daemon assumes: a single live CLI
    /// process (one PID) maps to at most one session. A long-lived
    /// `claude`/`codex` process rotates through several session ids over its
    /// lifetime (`/clear`, `--resume`, compaction), and a scanner-discovered
    /// `scan-<pid>` placeholder can linger alongside the hook UUID session for
    /// the same PID. `cleanup_dead` can't reap any of them — the PID is still
    /// alive with the agent's comm — so they pile up as ghost rows in the panel.
    /// Keep the most relevant session per PID and drop the rest.
    ///
    /// Window-backed agents are exempt: several editor windows legitimately
    /// share one GUI process PID, so their identity is the window id, not the
    /// PID.
    pub fn dedupe_cli_pids(&self) {
        let mut map = self.sessions.write().unwrap();
        // For each PID, track the best (tier, recency, id) seen so far. Higher
        // wins; the id tiebreaker keeps the choice stable across scans so the
        // panel doesn't flicker between equally-scored ghosts.
        let mut best: HashMap<u32, (u8, u64, String)> = HashMap::new();
        for (id, session) in map.iter() {
            if session.agent.is_window_backed() {
                continue;
            }
            let (tier, recency) = cli_keep_score(session);
            let candidate = (tier, recency, id.clone());
            match best.get(&session.pid) {
                Some(current) if *current >= candidate => {}
                _ => {
                    best.insert(session.pid, candidate);
                }
            }
        }
        let keep: HashSet<String> = best.into_values().map(|(_, _, id)| id).collect();
        map.retain(|id, session| session.agent.is_window_backed() || keep.contains(id));
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

/// Rank a CLI session for `dedupe_cli_pids`: higher is kept. A real hook
/// session (UUID id) outranks a `scan-<pid>` placeholder because it carries the
/// richer hook-driven state; within the same tier the most recently active
/// session wins, since that's the one the user is actually driving.
fn cli_keep_score(session: &Session) -> (u8, u64) {
    let tier = if session.id.starts_with("scan-") { 0 } else { 1 };
    let recency = [
        session.last_prompt_at,
        session.last_agent_text_at,
        session.last_tool_at,
        session.started_at_epoch,
    ]
    .into_iter()
    .flatten()
    .max()
    .unwrap_or(0);
    (tier, recency)
}

/// Check whether a PID is still occupied by a process of the given `AgentKind`,
/// using `/proc/<pid>/comm`. Returns false when `/proc/<pid>/comm` can't be
/// read (the process has exited, the PID slot is empty, or we lack
/// permission) and when the comm name doesn't match the expected comms for
/// that kind — which is how we distinguish a live Claude session from a
/// PID that has been recycled by an unrelated process.
pub fn is_agent_pid_alive(pid: u32, kind: AgentKind) -> bool {
    let Ok(comm) = std::fs::read_to_string(format!("/proc/{}/comm", pid)) else {
        return false;
    };
    is_agent_pid_alive_with_comm(&comm, kind)
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

/// How far up a process tree we walk before giving up on finding a window.
const PID_WALK_MAX_DEPTH: usize = 10;

/// A PID followed by its ancestors (bounded depth), stopping at init.
fn ancestry(pid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut cur = pid;
    for _ in 0..PID_WALK_MAX_DEPTH {
        out.push(cur);
        match parent_pid(cur) {
            Some(ppid) => cur = ppid,
            None => break,
        }
    }
    out
}

/// Read a process's `ZELLIJ_SESSION_NAME` from `/proc/<pid>/environ`.
/// Every process inside a Zellij session inherits this. Agents run under the
/// *persistent Zellij server* (not the terminal window), so their own process
/// ancestry never reaches the window — this is how we recover which session a
/// PID belongs to so we can find the matching client window. None outside Zellij.
pub fn zellij_session_of(pid: u32) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{}/environ", pid)).ok()?;
    raw.split(|b| *b == 0).find_map(|kv| {
        std::str::from_utf8(kv)
            .ok()?
            .strip_prefix("ZELLIJ_SESSION_NAME=")
            .map(str::to_string)
    })
    .filter(|s| !s.is_empty())
}

/// Find the Zellij *client* process for `session` — the one running inside a
/// terminal window, as opposed to the shared `--server` process whose ancestry
/// dead-ends at init. The client carries the session name as a bare argv token,
/// covering both launch forms: `zellij --session NAME …` and `zellij attach NAME`.
pub fn zellij_client_pid(session: &str) -> Option<u32> {
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        match std::fs::read_to_string(format!("/proc/{}/comm", pid)) {
            Ok(comm) if comm.trim() == "zellij" => {}
            _ => continue,
        }
        let Ok(raw) = std::fs::read(format!("/proc/{}/cmdline", pid)) else {
            continue;
        };
        let args: Vec<&str> = raw
            .split(|b| *b == 0)
            .filter_map(|a| std::str::from_utf8(a).ok())
            .filter(|a| !a.is_empty())
            .collect();
        // Skip the shared server; match a client that names this session.
        if args.iter().any(|a| *a == "--server") {
            continue;
        }
        if args.iter().any(|a| *a == session) {
            return Some(pid);
        }
    }
    None
}

/// A herdr pane identity read from a process's environment. Herdr injects
/// these into every pane it spawns; `socket_path` is the API socket of the
/// herdr session the pane belongs to, `pane_id` a `agent focus`-able target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HerdrPane {
    pub pane_id: String,
    pub socket_path: String,
}

/// Read a process's herdr pane identity from `/proc/<pid>/environ`.
/// Like Zellij, agents run under the persistent herdr *server*, so their
/// ancestry never reaches the terminal window — the env vars are how we
/// recover which herdr session/pane a PID belongs to. None outside herdr.
pub fn herdr_pane_of(pid: u32) -> Option<HerdrPane> {
    let raw = std::fs::read(format!("/proc/{}/environ", pid)).ok()?;
    herdr_pane_from_environ(&raw)
}

fn herdr_pane_from_environ(raw: &[u8]) -> Option<HerdrPane> {
    let mut pane_id = None;
    let mut socket_path = None;
    for kv in raw.split(|b| *b == 0) {
        let Ok(kv) = std::str::from_utf8(kv) else { continue };
        if let Some(v) = kv.strip_prefix("HERDR_PANE_ID=") {
            pane_id = Some(v.to_string());
        } else if let Some(v) = kv.strip_prefix("HERDR_SOCKET_PATH=") {
            socket_path = Some(v.to_string());
        }
    }
    Some(HerdrPane {
        pane_id: pane_id.filter(|s| !s.is_empty())?,
        socket_path: socket_path.filter(|s| !s.is_empty())?,
    })
}

/// Herdr session name derived from an API socket path. Named sessions live
/// at `…/sessions/<name>/herdr.sock`; anything else is the default session.
pub fn herdr_session_name(socket_path: &str) -> String {
    let dir = std::path::Path::new(socket_path).parent();
    if let (Some(dir), Some(grand)) = (dir, dir.and_then(|d| d.parent())) {
        if grand.file_name().is_some_and(|n| n == "sessions") {
            if let Some(name) = dir.file_name().and_then(|n| n.to_str()) {
                return name.to_string();
            }
        }
    }
    "default".to_string()
}

/// Does a herdr process's argv identify it as the *client* attached to
/// `session`? Only the exact launch forms count — anything else (the
/// `… server` process, transient `herdr <subcommand>` CLI calls, remote
/// attaches) must not match, or the scanner could resolve an agent to a
/// window hosting a short-lived CLI invocation instead of the real client.
fn herdr_client_args_match(args: &[&str], session: &str) -> bool {
    match args {
        [_] => session == "default",
        [_, "--session", name] => *name == session,
        [_, "session", "attach", name] => *name == session,
        _ => false,
    }
}

/// Find the herdr *client* process for `session` — the one running inside a
/// terminal window, as opposed to the shared server whose ancestry dead-ends
/// at init. Its ancestry is the branch that reaches the terminal window.
pub fn herdr_client_pid(session: &str) -> Option<u32> {
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        match std::fs::read_to_string(format!("/proc/{}/comm", pid)) {
            Ok(comm) if comm.trim() == "herdr" => {}
            _ => continue,
        }
        let Ok(raw) = std::fs::read(format!("/proc/{}/cmdline", pid)) else {
            continue;
        };
        let args: Vec<&str> = raw
            .split(|b| *b == 0)
            .filter_map(|a| std::str::from_utf8(a).ok())
            .filter(|a| !a.is_empty())
            .collect();
        if herdr_client_args_match(&args, session) {
            return Some(pid);
        }
    }
    None
}

/// Ordered candidate PIDs to match against compositor windows when locating the
/// terminal that hosts `agent_pid`. The agent's own ancestry comes first (covers
/// terminals running the agent as a direct child, e.g. kitty splits); if the
/// agent lives in a Zellij or herdr session, the matching client's ancestry is
/// appended — that's the branch that actually reaches the terminal window.
pub fn window_candidate_pids(agent_pid: u32) -> Vec<u32> {
    let mut pids = ancestry(agent_pid);
    if let Some(session) = zellij_session_of(agent_pid) {
        if let Some(client) = zellij_client_pid(&session) {
            pids.extend(ancestry(client));
        }
    }
    if let Some(pane) = herdr_pane_of(agent_pid) {
        if let Some(client) = herdr_client_pid(&herdr_session_name(&pane.socket_path)) {
            pids.extend(ancestry(client));
        }
    }
    pids
}

/// Detect which terminal hosts a process by walking up the process tree.
pub fn detect_terminal(pid: u32) -> String {
    let mut current = pid;
    for _ in 0..10 {
        if let Ok(comm) = std::fs::read_to_string(format!("/proc/{}/comm", current)) {
            match comm.trim() {
                // The herdr server sits between an agent and init, so the
                // walk can never reach the real terminal; name the muxer.
                "herdr" => return "Herdr".to_string(),
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

    /// A session with an explicit status, start time and last-activity stamp.
    fn ordered(id: &str, status: SessionStatus, started: u64, activity: u64) -> Session {
        let mut s = Session::new(id.into(), AgentKind::ClaudeCode, 1);
        s.status = status;
        s.started_at_epoch = Some(started);
        s.last_tool_at = Some(activity);
        s
    }

    #[test]
    fn sort_puts_approval_first_then_working_then_idle_then_stopped() {
        let mut sessions = vec![
            ordered("stopped", SessionStatus::Stopped, 10, 10),
            ordered("idle", SessionStatus::Idle, 10, 10),
            ordered("thinking", SessionStatus::Thinking, 10, 10),
            ordered("approval", SessionStatus::WaitingApproval, 10, 10),
        ];
        sort_by_activity(&mut sessions);
        let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["approval", "thinking", "idle", "stopped"]);
    }

    /// An idle session whose `Stop` landed at epoch `at`, still unacknowledged.
    fn finished(id: &str, at: u64) -> Session {
        let mut s = ordered(id, SessionStatus::Idle, 10, 10);
        s.finished_at = Some(at);
        s
    }

    #[test]
    fn just_finished_holds_until_the_card_is_clicked() {
        let mut s = finished("done", 100);
        assert!(s.just_finished(), "stays lit however long it waits");
        s.acknowledge();
        assert!(!s.just_finished(), "the click clears it");
        // Never stamped at all — a scanner-discovered idle session.
        assert!(!ordered("never-ran", SessionStatus::Idle, 10, 10).just_finished());
    }

    #[test]
    fn mark_finished_bumps_the_sequence_every_turn() {
        // What the debounced idle announcement is guarded on: a second turn
        // ending inside the delay window has to be distinguishable from the
        // first, which `finished_at` alone cannot do — it is in whole seconds,
        // so both turns can carry the very same stamp.
        let mut s = Session::new("seq".into(), AgentKind::ClaudeCode, 1);
        assert_eq!(s.finish_seq, 0, "nothing has finished yet");
        s.mark_finished();
        let first = s.finish_seq;
        assert_eq!(first, 1);
        s.status = SessionStatus::Thinking;
        s.mark_finished();
        assert_ne!(
            s.finish_seq, first,
            "a later turn must invalidate the earlier pending announcement"
        );
        assert_eq!(s.finished_at, Some(now_epoch()), "still stamps the finish");
    }

    #[test]
    fn spawns_subagent_matches_both_names_only() {
        assert!(spawns_subagent(TOOL_AGENT));
        assert!(spawns_subagent(TOOL_TASK));
        // An ordinary tool must not inflate the count: one bogus increment holds
        // the session's announcements until the next user prompt resets it.
        for tool in ["Bash", "Read", "WebFetch", "agent", ""] {
            assert!(!spawns_subagent(tool), "{tool} should not count");
        }
    }

    #[test]
    fn subagent_counting_only_releases_on_the_last_one() {
        let mut s = Session::new("fanout".into(), AgentKind::ClaudeCode, 1);
        s.launch_subagent();
        s.launch_subagent();
        assert!(!s.subagent_finished(), "one still running");
        assert!(s.subagent_finished(), "that was the last one");
        // A late or duplicate report must not release a second time — that would
        // schedule a second announcement for an already-announced turn.
        assert!(!s.subagent_finished(), "count already empty");
        assert_eq!(s.pending_agents, 0, "and must not underflow");
    }

    #[test]
    fn reset_subagents_reports_what_it_dropped() {
        let mut s = Session::new("wedged".into(), AgentKind::ClaudeCode, 1);
        s.launch_subagent();
        s.launch_subagent();
        assert_eq!(s.reset_subagents(), 2);
        assert_eq!(s.reset_subagents(), 0, "nothing left to drop");
    }

    #[test]
    fn announceable_needs_idle_finished_and_nothing_outstanding() {
        let mut s = Session::new("turn".into(), AgentKind::ClaudeCode, 1);
        // Never stopped: nothing to announce.
        assert!(!s.announceable());
        s.mark_finished();
        assert!(s.announceable(), "stopped, idle, nothing running");
        // Waiting on work it launched is not a finish, however quiet it looks.
        s.launch_subagent();
        assert!(!s.announceable());
        s.subagent_finished();
        assert!(s.announceable());
        // Back at work: the turn moved on.
        s.status = SessionStatus::Executing;
        assert!(!s.announceable());
    }

    #[test]
    fn a_held_turn_still_reads_as_finished_so_the_ceiling_can_release_it() {
        // The shape the hold ceiling keys on: a turn waiting on sub-agents is not
        // announceable, but it is still `just_finished` and still on the same
        // `finish_seq`. That combination is what tells "held" apart from "the
        // session moved on", and it is why dropping the count is enough to
        // release the announcement once the sub-agents are presumed gone.
        let mut s = Session::new("held".into(), AgentKind::ClaudeCode, 1);
        s.launch_subagent();
        s.mark_finished();
        let held_on = s.finish_seq;
        assert!(!s.announceable(), "held: nothing to announce yet");
        assert!(s.just_finished(), "but the turn did end, and stays lit");
        assert_eq!(s.reset_subagents(), 1, "the ceiling presumes them gone");
        assert!(s.announceable(), "which releases the announcement");
        assert_eq!(s.finish_seq, held_on, "still the very turn that was held");
    }

    #[test]
    fn just_finished_clears_as_soon_as_the_agent_works_again() {
        let mut s = finished("back-to-work", 100);
        s.status = SessionStatus::Thinking;
        assert!(!s.just_finished());
    }

    #[test]
    fn acknowledge_leaves_a_pending_approval_alone() {
        // Clicking the card means "I'm heading over there", not "allow" —
        // the prompt must still be answered somewhere.
        let mut s = ordered("asking", SessionStatus::WaitingApproval, 10, 10);
        s.pending_approval = Some(PendingApproval {
            request_id: "r1".into(),
            tool: "Bash".into(),
            detail: None,
            choices: vec![],
        });
        s.acknowledge();
        assert!(s.pending_approval.is_some());
        assert_eq!(s.status, SessionStatus::WaitingApproval);
    }

    #[test]
    fn sort_puts_just_finished_under_approval_and_over_working() {
        let mut sessions = vec![
            ordered("idle", SessionStatus::Idle, 10, 10),
            ordered("thinking", SessionStatus::Thinking, 10, 10),
            finished("just-finished", 100),
            ordered("approval", SessionStatus::WaitingApproval, 10, 10),
        ];
        sort_by_activity(&mut sessions);
        let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["approval", "just-finished", "thinking", "idle"]);
    }

    #[test]
    fn sort_orders_just_finished_sessions_freshest_first() {
        let mut sessions = vec![
            ordered("thinking", SessionStatus::Thinking, 10, 10),
            finished("older", 100),
            finished("newest", 200),
        ];
        sort_by_activity(&mut sessions);
        let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["newest", "older", "thinking"]);
    }

    #[test]
    fn acknowledged_session_falls_back_into_the_idle_band() {
        let mut sessions = vec![
            ordered("thinking", SessionStatus::Thinking, 10, 10),
            finished("clicked", 100),
        ];
        sessions[1].acknowledge();
        sort_by_activity(&mut sessions);
        let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["thinking", "clicked"]);
    }

    #[test]
    fn sort_bands_executing_with_thinking_and_running_with_idle() {
        let mut sessions = vec![
            ordered("running", SessionStatus::Running, 10, 10),
            ordered("executing", SessionStatus::Executing, 10, 10),
        ];
        sort_by_activity(&mut sessions);
        let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["executing", "running"]);
    }

    #[test]
    fn sort_orders_idle_sessions_most_recently_active_first() {
        let mut sessions = vec![
            ordered("old", SessionStatus::Idle, 1, 100),
            ordered("fresh", SessionStatus::Idle, 1, 300),
            ordered("middle", SessionStatus::Idle, 1, 200),
        ];
        sort_by_activity(&mut sessions);
        let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["fresh", "middle", "old"]);
    }

    #[test]
    fn sort_orders_working_sessions_by_start_time_not_activity() {
        // Live rows must not reshuffle as their tool timestamps tick: the
        // newest *session* stays on top even though the older one just did
        // something more recently.
        let mut sessions = vec![
            ordered("started-first", SessionStatus::Executing, 100, 900),
            ordered("started-last", SessionStatus::Executing, 200, 500),
        ];
        sort_by_activity(&mut sessions);
        let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["started-last", "started-first"]);
    }

    #[test]
    fn sort_is_stable_on_identical_timestamps() {
        let mut sessions = vec![
            ordered("b", SessionStatus::Idle, 10, 10),
            ordered("a", SessionStatus::Idle, 10, 10),
        ];
        sort_by_activity(&mut sessions);
        let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
    }

    #[test]
    fn last_activity_takes_the_newest_stamp() {
        let mut s = Session::new("s1".into(), AgentKind::ClaudeCode, 1);
        s.started_at_epoch = Some(100);
        s.last_prompt_at = Some(400);
        s.last_tool_at = Some(250);
        assert_eq!(s.last_activity_at(), 400);
    }

    #[test]
    fn last_activity_falls_back_to_start_time() {
        let mut s = Session::new("s1".into(), AgentKind::ClaudeCode, 1);
        s.started_at_epoch = Some(100);
        assert_eq!(s.last_activity_at(), 100);
    }

    #[test]
    fn registry_all_by_activity_is_ordered() {
        let registry = SessionRegistry::new();
        registry.register(ordered("idle", SessionStatus::Idle, 10, 10));
        registry.register(ordered("approval", SessionStatus::WaitingApproval, 10, 10));
        let ids: Vec<String> = registry
            .all_by_activity()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, ["approval", "idle"]);
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

    #[test]
    fn is_agent_pid_alive_with_comm_matches() {
        assert!(is_agent_pid_alive_with_comm("claude", AgentKind::ClaudeCode));
        assert!(is_agent_pid_alive_with_comm("codex", AgentKind::Codex));
    }

    #[test]
    fn is_agent_pid_alive_with_comm_is_case_insensitive() {
        assert!(is_agent_pid_alive_with_comm("Claude", AgentKind::ClaudeCode));
        assert!(is_agent_pid_alive_with_comm("CODEX", AgentKind::Codex));
    }

    #[test]
    fn is_agent_pid_alive_with_comm_trims_whitespace() {
        // /proc/<pid>/comm always has a trailing newline.
        assert!(is_agent_pid_alive_with_comm("claude\n", AgentKind::ClaudeCode));
        assert!(is_agent_pid_alive_with_comm("  claude  ", AgentKind::ClaudeCode));
    }

    #[test]
    fn is_agent_pid_alive_with_comm_rejects_mismatch() {
        assert!(!is_agent_pid_alive_with_comm("zsh", AgentKind::ClaudeCode));
        assert!(!is_agent_pid_alive_with_comm("git", AgentKind::Codex));
        assert!(!is_agent_pid_alive_with_comm("", AgentKind::ClaudeCode));
    }

    #[test]
    fn is_agent_pid_alive_with_comm_rejects_window_agents() {
        // Cursor/WebStorm have no comm list, so no comm can ever match.
        assert!(!is_agent_pid_alive_with_comm("cursor", AgentKind::Cursor));
        assert!(!is_agent_pid_alive_with_comm("idea", AgentKind::WebStorm));
    }

    #[test]
    fn is_agent_pid_alive_rejects_non_agent_pid() {
        // PID 1 is init/systemd on Linux — alive, but comm is not "claude".
        assert!(!is_agent_pid_alive(1, AgentKind::ClaudeCode));
        assert!(!is_agent_pid_alive(1, AgentKind::Codex));
    }

    #[test]
    fn is_agent_pid_alive_rejects_dead_pid() {
        // A very high PID is almost certainly not a live process.
        assert!(!is_agent_pid_alive(4_000_000, AgentKind::ClaudeCode));
    }

    #[test]
    fn is_agent_pid_alive_rejects_window_agents_unconditionally() {
        // Cursor/WebStorm have no /proc-based liveness. PID 1 is alive but
        // should still report false because expected_comms_for returns [].
        assert!(!is_agent_pid_alive(1, AgentKind::Cursor));
        assert!(!is_agent_pid_alive(1, AgentKind::WebStorm));
    }

    #[test]
    fn cleanup_dead_drops_hook_session_with_non_agent_pid() {
        // PID 1 (init) is alive but its comm is not "claude" — simulates a
        // ghost session after PID reuse.
        let registry = SessionRegistry::new();
        registry.register(Session::new(
            "11111111-2222-3333-4444-555555555555".into(),
            AgentKind::ClaudeCode,
            1,
        ));
        registry.cleanup_dead();
        assert!(registry.all().is_empty(), "ghost session should be evicted");
    }

    #[test]
    fn cleanup_dead_drops_scan_session_with_dead_pid() {
        let registry = SessionRegistry::new();
        registry.register(Session::new(
            "scan-claude-4000000".into(),
            AgentKind::ClaudeCode,
            4_000_000,
        ));
        registry.cleanup_dead();
        assert!(registry.all().is_empty());
    }

    #[test]
    fn window_candidate_pids_starts_with_the_agent_pid() {
        // Our own PID is real, so its ancestry is non-empty and leads with it.
        let me = std::process::id();
        let pids = window_candidate_pids(me);
        assert_eq!(pids.first(), Some(&me));
    }

    #[test]
    fn zellij_session_of_is_none_for_init() {
        // PID 1 (init) is not inside any Zellij session.
        assert_eq!(zellij_session_of(1), None);
    }

    #[test]
    fn zellij_client_pid_is_none_for_bogus_session() {
        // A session name that cannot exist must not match the server or anything.
        assert_eq!(
            zellij_client_pid("vibewatch-no-such-session-zzz-9999"),
            None
        );
    }

    #[test]
    fn herdr_pane_from_environ_reads_pane_and_socket() {
        let raw = b"HOME=/home/u\0HERDR_PANE_ID=w8:p1\0HERDR_TAB_ID=w8:t1\0HERDR_SOCKET_PATH=/home/u/.config/herdr/herdr.sock\0";
        let pane = herdr_pane_from_environ(raw).unwrap();
        assert_eq!(pane.pane_id, "w8:p1");
        assert_eq!(pane.socket_path, "/home/u/.config/herdr/herdr.sock");
    }

    #[test]
    fn herdr_pane_from_environ_is_none_without_herdr_vars() {
        assert_eq!(herdr_pane_from_environ(b"HOME=/home/u\0SHELL=zsh\0"), None);
    }

    #[test]
    fn herdr_pane_from_environ_requires_both_vars() {
        assert_eq!(herdr_pane_from_environ(b"HERDR_PANE_ID=w1:p1\0"), None);
        assert_eq!(
            herdr_pane_from_environ(b"HERDR_SOCKET_PATH=/tmp/h.sock\0"),
            None
        );
        assert_eq!(
            herdr_pane_from_environ(b"HERDR_PANE_ID=\0HERDR_SOCKET_PATH=/tmp/h.sock\0"),
            None
        );
    }

    #[test]
    fn herdr_pane_of_is_none_for_init() {
        assert_eq!(herdr_pane_of(1), None);
    }

    #[test]
    fn herdr_session_name_default_for_top_level_socket() {
        assert_eq!(
            herdr_session_name("/home/u/.config/herdr/herdr.sock"),
            "default"
        );
    }

    #[test]
    fn herdr_session_name_reads_named_session_dir() {
        assert_eq!(
            herdr_session_name("/home/u/.config/herdr/sessions/mbrella/herdr.sock"),
            "mbrella"
        );
    }

    #[test]
    fn herdr_client_args_match_plain_client_is_default() {
        assert!(herdr_client_args_match(&["herdr"], "default"));
        assert!(!herdr_client_args_match(&["herdr"], "mbrella"));
    }

    #[test]
    fn herdr_client_args_match_named_session_forms() {
        assert!(herdr_client_args_match(
            &["herdr", "--session", "mbrella"],
            "mbrella"
        ));
        assert!(herdr_client_args_match(
            &["herdr", "session", "attach", "mbrella"],
            "mbrella"
        ));
        assert!(!herdr_client_args_match(
            &["herdr", "--session", "o27"],
            "mbrella"
        ));
    }

    #[test]
    fn herdr_client_args_match_rejects_servers_and_cli_calls() {
        assert!(!herdr_client_args_match(&["herdr", "server"], "default"));
        assert!(!herdr_client_args_match(
            &["herdr", "--session", "mbrella", "server"],
            "mbrella"
        ));
        assert!(!herdr_client_args_match(
            &["herdr", "workspace", "list"],
            "default"
        ));
        assert!(!herdr_client_args_match(
            &["herdr", "agent", "focus", "w8:p1"],
            "default"
        ));
        assert!(!herdr_client_args_match(
            &["herdr", "--remote", "host"],
            "default"
        ));
    }

    #[test]
    fn herdr_client_pid_is_none_for_bogus_session() {
        assert_eq!(
            herdr_client_pid("vibewatch-no-such-session-zzz-9999"),
            None
        );
    }

    #[test]
    fn dedupe_cli_pids_keeps_one_session_per_pid() {
        // Same live PID rotated through three session ids (/clear, resume,
        // compaction). Only the most recent should survive.
        let registry = SessionRegistry::new();
        let mut a = Session::new("uuid-old".into(), AgentKind::ClaudeCode, 4242);
        a.started_at_epoch = Some(100);
        a.last_prompt_at = Some(100);
        let mut b = Session::new("uuid-mid".into(), AgentKind::ClaudeCode, 4242);
        b.started_at_epoch = Some(200);
        b.last_prompt_at = Some(200);
        let mut c = Session::new("uuid-new".into(), AgentKind::ClaudeCode, 4242);
        c.started_at_epoch = Some(300);
        c.last_prompt_at = Some(300);
        registry.register(a);
        registry.register(b);
        registry.register(c);

        registry.dedupe_cli_pids();

        let all = registry.all();
        assert_eq!(all.len(), 1, "one session should remain for the PID");
        assert_eq!(all[0].id, "uuid-new", "most recently active wins");
    }

    #[test]
    fn dedupe_cli_pids_prefers_hook_session_over_scan_placeholder() {
        // A scan- placeholder lingering next to the real hook session for the
        // same PID — keep the hook session even when the scan one looks newer.
        let registry = SessionRegistry::new();
        let mut scan = Session::new("scan-claude-555".into(), AgentKind::ClaudeCode, 555);
        scan.started_at_epoch = Some(9999);
        let mut hook = Session::new("real-uuid".into(), AgentKind::ClaudeCode, 555);
        hook.started_at_epoch = Some(1);
        registry.register(scan);
        registry.register(hook);

        registry.dedupe_cli_pids();

        let all = registry.all();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, "real-uuid", "hook session outranks scan placeholder");
    }

    #[test]
    fn dedupe_cli_pids_leaves_distinct_pids_untouched() {
        let registry = SessionRegistry::new();
        registry.register(Session::new("a".into(), AgentKind::ClaudeCode, 1));
        registry.register(Session::new("b".into(), AgentKind::ClaudeCode, 2));
        registry.register(Session::new("c".into(), AgentKind::Codex, 3));

        registry.dedupe_cli_pids();

        assert_eq!(registry.all().len(), 3);
    }

    #[test]
    fn dedupe_cli_pids_exempts_window_agents_sharing_a_pid() {
        // Multiple editor windows legitimately share one GUI process PID; their
        // identity is the window id, so dedup must not collapse them.
        let registry = SessionRegistry::new();
        registry.register(Session::new("window-cursor-1".into(), AgentKind::Cursor, 700));
        registry.register(Session::new("window-cursor-2".into(), AgentKind::Cursor, 700));

        registry.dedupe_cli_pids();

        assert_eq!(registry.all().len(), 2);
    }

    #[test]
    fn cleanup_dead_retains_window_session_regardless_of_pid() {
        // Window sessions are reaped by the compositor scan; cleanup_dead
        // must not touch them even when the PID is clearly dead.
        let registry = SessionRegistry::new();
        registry.register(Session::new(
            "window-cursor-xyz".into(),
            AgentKind::Cursor,
            4_000_000,
        ));
        registry.cleanup_dead();
        assert_eq!(registry.all().len(), 1);
    }
}
