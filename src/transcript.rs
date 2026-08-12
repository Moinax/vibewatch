//! Per-agent transcript parsing: find the last assistant text line, and reduce
//! a Claude Code transcript to the live state the hooks would have reported.

use crate::session::{AgentKind, SessionStatus};
use serde_json::Value;
use std::collections::HashSet;
use std::io::{BufRead, Read, Seek};
use std::path::{Path, PathBuf};

/// Read the last assistant text line from the session's transcript file.
///
/// Returns `None` for agents without an accessible transcript (Cursor, WebStorm),
/// if the file cannot be located, if it contains no assistant text, or if the
/// final text line is empty or is only a code fence.
///
/// `cached_path` is used to avoid re-walking the filesystem on every call; on a
/// successful read it is populated with the resolved path.
pub fn read_last_assistant_line(
    agent: AgentKind,
    session_id: &str,
    cached_path: &mut Option<PathBuf>,
) -> Option<String> {
    match agent {
        AgentKind::Cursor | AgentKind::WebStorm => None,
        AgentKind::ClaudeCode => {
            let home = dirs::home_dir()?;
            read_last_assistant_line_in(agent, &home.join(".claude"), session_id, cached_path)
        }
        AgentKind::Codex => {
            let home = dirs::home_dir()?;
            read_last_assistant_line_in(agent, &home.join(".codex"), session_id, cached_path)
        }
    }
}

/// Walk `<root>/projects/*/` looking for `<session_id>.jsonl`.
fn resolve_claude_path_in(root: &Path, session_id: &str) -> Option<PathBuf> {
    let projects = root.join("projects");
    for project in std::fs::read_dir(&projects).ok()?.flatten() {
        let candidate = project.path().join(format!("{}.jsonl", session_id));
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Return the last non-empty, non-code-fence-only line of `text`.
fn last_non_empty_line(text: &str) -> Option<String> {
    for line in text.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if is_code_fence(trimmed) {
            continue;
        }
        return Some(trimmed.to_string());
    }
    None
}

/// True for markdown code-fence lines: bare ```` ``` ```` or language-tagged
/// ```` ```lang ```` (alphanumeric/`-`/`_` remainder, no spaces).
fn is_code_fence(trimmed: &str) -> bool {
    trimmed == "```"
        || (trimmed.starts_with("```")
            && !trimmed.contains(' ')
            && trimmed
                .chars()
                .skip(3)
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
}

/// Parse a Claude JSONL file and return the last non-empty assistant text line.
/// Iterates lines from the end; the first line whose assistant `content` contains
/// at least one text block wins. Returns `None` if no such line exists.
fn parse_claude(content: &str) -> Option<String> {
    for line in content.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let msg = value.get("message").unwrap_or(&value);
        if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }
        let content_arr = match msg.get("content").and_then(|c| c.as_array()) {
            Some(a) => a,
            None => continue,
        };
        let mut joined = String::new();
        for block in content_arr {
            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    if !joined.is_empty() {
                        joined.push('\n');
                    }
                    joined.push_str(text);
                }
            }
        }
        if joined.is_empty() {
            continue;
        }
        if let Some(last) = last_non_empty_line(&joined) {
            return Some(last);
        }
    }
    None
}

/// Parse a Codex JSONL file and return the last non-empty assistant text line.
fn parse_codex(content: &str) -> Option<String> {
    for line in content.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if value.get("type").and_then(|t| t.as_str()) != Some("response_item") {
            continue;
        }
        let payload = match value.get("payload") {
            Some(p) => p,
            None => continue,
        };
        if payload.get("type").and_then(|t| t.as_str()) != Some("message")
            || payload.get("role").and_then(|r| r.as_str()) != Some("assistant")
        {
            continue;
        }
        let content_arr = match payload.get("content").and_then(|c| c.as_array()) {
            Some(a) => a,
            None => continue,
        };
        let mut joined = String::new();
        for block in content_arr {
            if block.get("type").and_then(|t| t.as_str()) == Some("output_text") {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    if !joined.is_empty() {
                        joined.push('\n');
                    }
                    joined.push_str(text);
                }
            }
        }
        if joined.is_empty() {
            continue;
        }
        if let Some(last) = last_non_empty_line(&joined) {
            return Some(last);
        }
    }
    None
}

/// Testable variant of `read_last_assistant_line` that accepts an explicit
/// `.claude`-equivalent root directory.
pub(crate) fn read_last_assistant_line_in(
    agent: AgentKind,
    root: &Path,
    session_id: &str,
    cached_path: &mut Option<PathBuf>,
) -> Option<String> {
    match agent {
        AgentKind::Cursor | AgentKind::WebStorm => None,
        AgentKind::ClaudeCode => {
            let path = match cached_path {
                Some(p) if p.exists() => p.clone(),
                _ => {
                    let resolved = resolve_claude_path_in(root, session_id)?;
                    *cached_path = Some(resolved.clone());
                    resolved
                }
            };
            parse_claude(&head_and_tail(&path)?)
        }
        AgentKind::Codex => {
            let path = match cached_path {
                Some(p) if p.exists() => p.clone(),
                _ => {
                    let resolved = resolve_codex_path_in(root, session_id)?;
                    *cached_path = Some(resolved.clone());
                    resolved
                }
            };
            parse_codex(&head_and_tail(&path)?)
        }
    }
}

/// Walk `<root>/sessions` recursively for a file named `*-<session_id>.jsonl`.
fn resolve_codex_path_in(root: &Path, session_id: &str) -> Option<PathBuf> {
    let sessions = root.join("sessions");
    let suffix = format!("-{}.jsonl", session_id);
    walk_for_suffix(&sessions, &suffix)
}

fn walk_for_suffix(dir: &Path, suffix: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = walk_for_suffix(&path, suffix) {
                return Some(found);
            }
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.ends_with(suffix))
            .unwrap_or(false)
        {
            return Some(path);
        }
    }
    None
}

/// What a Claude Code transcript says a session is doing, as of its last line.
///
/// Hooks are the source of truth for this and the daemon holds the result in
/// memory. The reduction exists for the moments there is no memory to hold it:
/// the daemon restarting mid-session, or starting after the agent did. A
/// freshly discovered session begins at `Idle`, and an agent blocked on a
/// question emits no further hooks — so without this, nothing ever corrects
/// that guess and the widget reads "idle" until the user answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeSnapshot {
    pub session_id: String,
    pub status: SessionStatus,
    pub current_tool: Option<String>,
    pub tool_detail: Option<String>,
}

/// Tools that hand the turn to the user the moment they run: the transcript
/// shows the `tool_use` and then nothing until an answer arrives, so a pending
/// one is an agent waiting, not an agent working.
///
/// Every other pending tool reduces to `Executing`. A permission prompt on a
/// Bash call looks identical to that Bash call still running — Claude Code
/// records the prompt nowhere — so "running" is the most this can honestly say.
fn waits_on_the_user(tool: &str) -> bool {
    tool == crate::session::TOOL_ASK_USER_QUESTION || tool == crate::session::TOOL_EXIT_PLAN_MODE
}

/// Reduce Claude Code transcript JSONL to the state it ends in.
///
/// Folds forward rather than reading backwards: only the final state matters,
/// and a turn is a run of records (thinking, text, several tool calls) whose
/// meaning depends on what came before it in the turn.
pub fn reduce_claude(content: &str) -> Option<ClaudeSnapshot> {
    let fold = fold_claude(content, true);
    Some(ClaudeSnapshot {
        session_id: fold.session_id?,
        status: fold.status,
        current_tool: fold.current_tool,
        tool_detail: fold.tool_detail,
    })
}

/// What [`fold_claude`] accumulates: [`ClaudeSnapshot`] minus the demand that a
/// session id was ever seen, which a sub-agent's transcript cannot meet.
struct ClaudeFold {
    session_id: Option<String>,
    status: SessionStatus,
    current_tool: Option<String>,
    tool_detail: Option<String>,
}

/// The fold behind [`reduce_claude`], with the sidechain rule as a switch: in a
/// parent's transcript a sidechain record is someone else's work and must be
/// skipped, but a sub-agent's own file marks *every* record `isSidechain` —
/// skipping them there folds the whole file away.
fn fold_claude(content: &str, skip_sidechain: bool) -> ClaudeFold {
    let mut session_id: Option<String> = None;
    let mut status = SessionStatus::Idle;
    let mut current_tool: Option<String> = None;
    let mut tool_detail: Option<String> = None;
    // tool_use ids still awaiting a tool_result.
    let mut pending: HashSet<String> = HashSet::new();

    for line in content.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if session_id.is_none() {
            session_id = record
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        // A sub-agent's turns are recorded in its parent's transcript. That is
        // the sub-agent's work, not what this pane is doing.
        if skip_sidechain && record.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        // Injected context (system reminders and the like) wears the user role
        // without a user having typed anything.
        if record.get("isMeta").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let message = record.get("message").unwrap_or(&record);
        let blocks = message.get("content").and_then(Value::as_array);
        match message.get("role").and_then(Value::as_str) {
            Some("assistant") => {
                let Some(blocks) = blocks else { continue };
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("tool_use") => {
                            let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                            if let Some(id) = block.get("id").and_then(Value::as_str) {
                                pending.insert(id.to_owned());
                            }
                            status = if waits_on_the_user(name) {
                                SessionStatus::WaitingApproval
                            } else {
                                SessionStatus::Executing
                            };
                            current_tool = Some(name.to_owned());
                            tool_detail = block.get("input").and_then(tool_detail_from_input);
                        }
                        // Text with nothing outstanding is the turn ending. With a
                        // tool still pending it is the preamble to that tool call,
                        // and `thinking` blocks say nothing either way.
                        Some("text") if pending.is_empty() => {
                            status = SessionStatus::Idle;
                            current_tool = None;
                            tool_detail = None;
                        }
                        _ => {}
                    }
                }
            }
            Some("user") => {
                let results: Vec<&str> = blocks
                    .map(|bs| {
                        bs.iter()
                            .filter(|b| {
                                b.get("type").and_then(Value::as_str) == Some("tool_result")
                            })
                            .filter_map(|b| b.get("tool_use_id").and_then(Value::as_str))
                            .collect()
                    })
                    .unwrap_or_default();
                if results.is_empty() {
                    // A prompt — content is a bare string, or blocks of text and
                    // images. The agent is on it.
                    status = SessionStatus::Thinking;
                    current_tool = None;
                    tool_detail = None;
                    pending.clear();
                    continue;
                }
                for id in results {
                    pending.remove(id);
                }
                // Parallel tool calls each get their own result; the turn is only
                // back to the model once the last of them lands.
                if pending.is_empty() {
                    status = SessionStatus::Thinking;
                    current_tool = None;
                    tool_detail = None;
                }
            }
            _ => {}
        }
    }

    ClaudeFold {
        session_id,
        status,
        current_tool,
        tool_detail,
    }
}

/// Count the sub-agents of the session at `transcript_path` that are still
/// mid-turn, judged by their own transcripts under
/// `<session-id>/subagents/agent-*.jsonl`.
///
/// This exists for daemon restarts: the count of sub-agents holding a turn open
/// is hook-fed, and launches that predate the daemon are never re-announced —
/// only the `SubagentStop` per finished agent arrives, decrementing a count
/// that was never incremented. Recounting off the durable record instead means
/// the boot-time answer is right no matter what happened while no daemon was
/// listening, which no persisted registry could promise.
///
/// `not_before` is the same mtime floor the parent transcript passed
/// ([`find_claude_transcript_for_cwd`]): a resumed session keeps its id, so the
/// directory can hold agents belonging to an earlier process — including one
/// that died mid-turn, which would otherwise be counted outstanding forever.
/// A live one that dies later without reporting is the same case the hooks
/// already have, bounded by the hold ceiling and reset on the next prompt.
pub fn count_outstanding_subagents(
    transcript_path: &Path,
    not_before: std::time::SystemTime,
) -> u32 {
    let Some(stem) = transcript_path.file_stem() else {
        return 0;
    };
    let dir = transcript_path.with_file_name(stem).join("subagents");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut outstanding = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("agent-") || !name.ends_with(".jsonl") {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if modified < not_before {
            continue;
        }
        let Some(content) = head_and_tail(&entry.path()) else {
            continue;
        };
        if fold_claude(&content, false).status != SessionStatus::Idle {
            outstanding += 1;
        }
    }
    outstanding
}

/// One line of context for the tool a session is sitting in, pulled from its
/// input. `AskUserQuestion` is asked first because the question itself is the
/// only useful thing about it.
///
/// Shared with the Codex reducer — see [`tool_detail_from_input`] for why the
/// key list is a union rather than one list per agent.
pub fn tool_detail_from_input(input: &Value) -> Option<String> {
    /// Keys that carry the interesting argument, most specific first. One list
    /// across both agents: the names barely overlap (`cmd` is Codex's, `pattern`
    /// is Claude's), and a tool whose input happens to answer to another agent's
    /// key gives a better detail than no detail at all.
    const KEYS: &[&str] = &[
        "cmd",
        "command",
        "file_path",
        "path",
        "pattern",
        "query",
        "prompt",
        "description",
        "message",
        "task_name",
    ];
    // Each key is asked for a *string*, so a tool whose `command` is an array
    // falls through to the next key rather than giving up on the whole input.
    let raw = input
        .get("questions")
        .and_then(Value::as_array)
        .and_then(|qs| qs.first())
        .and_then(|q| q.get("question"))
        .and_then(Value::as_str)
        .or_else(|| {
            KEYS.iter()
                .find_map(|key| input.get(key).and_then(Value::as_str))
        })?;
    Some(
        raw.lines()
            .next()
            .unwrap_or(raw)
            .chars()
            .take(160)
            .collect(),
    )
}

/// The head and tail of `path` as one string, skipping the middle on a large
/// file. `None` if it cannot be opened or read.
///
/// Every reader here folds a JSONL record stream and only cares about how it
/// ends, so the middle of a multi-megabyte transcript is pure read-and-discard.
/// The first line is kept whatever the size: it carries the session id on a
/// transcript whose later records have scrolled out of the window.
///
/// The partial record the seek lands in is dropped, so the result is always a
/// sequence of whole lines — with a gap in it, which every caller tolerates
/// because it parses line by line and skips what does not parse.
pub fn head_and_tail(path: &Path) -> Option<String> {
    const TAIL_BYTES: u64 = 512 * 1024;
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();

    if len <= TAIL_BYTES {
        let mut content = String::new();
        file.read_to_string(&mut content).ok()?;
        return Some(content);
    }

    let mut first = String::new();
    std::io::BufReader::new(file.try_clone().ok()?)
        .read_line(&mut first)
        .ok()?;
    file.seek(std::io::SeekFrom::Start(len - TAIL_BYTES)).ok()?;
    let mut tail = String::new();
    file.read_to_string(&mut tail).ok()?;
    // The seek probably landed in the middle of a record.
    first.push_str(tail.split_once('\n').map(|(_, rest)| rest).unwrap_or(""));
    Some(first)
}

/// Reduce the transcript at `path`, reading only its head and tail on large
/// files.
pub fn snapshot_claude_file(path: &Path) -> Option<ClaudeSnapshot> {
    reduce_claude(&head_and_tail(path)?)
}

/// Claude Code's project directory name for `cwd`: the path with every
/// non-alphanumeric character replaced by a dash.
fn project_dir_name(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Resolve `<projects_root>/<mangled cwd>`, the directory Claude Code keeps a
/// project's transcripts in.
///
/// Falls back to comparing existing directory names put through the same
/// mangling, so a rule that differs from ours on some character still resolves
/// — the names are lossy, and only Claude Code knows the exact rule.
fn project_dir_for_cwd(projects_root: &Path, cwd: &Path) -> Option<PathBuf> {
    let want = project_dir_name(cwd);
    let direct = projects_root.join(&want);
    if direct.is_dir() {
        return Some(direct);
    }
    for entry in std::fs::read_dir(projects_root).ok()?.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if project_dir_name(Path::new(&*entry.file_name().to_string_lossy())) == want {
            return Some(path);
        }
    }
    None
}

/// The transcript a live agent in `cwd` is writing: the most recently touched
/// one in its project directory, ignoring anything untouched since the process
/// started.
///
/// That mtime floor is what keeps a long-dead session's transcript — there are
/// dozens per project — from being pinned on a process that has written
/// nothing yet.
pub fn find_claude_transcript_for_cwd(
    projects_root: &Path,
    cwd: &Path,
    not_before: std::time::SystemTime,
) -> Option<PathBuf> {
    let dir = project_dir_for_cwd(projects_root, cwd)?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if modified < not_before {
            continue;
        }
        if best.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
            best = Some((modified, path));
        }
    }
    best.map(|(_, path)| path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_and_webstorm_return_none() {
        let mut p = None;
        assert!(read_last_assistant_line(AgentKind::Cursor, "s1", &mut p).is_none());
        assert!(read_last_assistant_line(AgentKind::WebStorm, "s1", &mut p).is_none());
        assert!(p.is_none());
    }

    #[test]
    fn claude_path_found_for_known_session() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/transcripts/claude");
        let id = "cafe1234-0000-0000-0000-000000000001";
        let path = resolve_claude_path_in(&root, id).expect("path resolves");
        assert!(path.ends_with("-test-project/cafe1234-0000-0000-0000-000000000001.jsonl"));
    }

    #[test]
    fn claude_path_none_for_unknown_session() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/transcripts/claude");
        assert!(resolve_claude_path_in(&root, "nonexistent-id").is_none());
    }

    fn claude_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transcripts/claude")
    }

    #[test]
    fn claude_ends_with_text_returns_last_non_empty_line() {
        let got = read_last_assistant_line_in(
            AgentKind::ClaudeCode,
            &claude_root(),
            "cafe1234-0000-0000-0000-000000000002",
            &mut None,
        );
        assert_eq!(got.as_deref(), Some("Starting now."));
    }

    #[test]
    fn claude_ends_with_tool_use_falls_back_to_earlier_text() {
        let got = read_last_assistant_line_in(
            AgentKind::ClaudeCode,
            &claude_root(),
            "cafe1234-0000-0000-0000-000000000003",
            &mut None,
        );
        // The most recent assistant message is text-less; next-most-recent has text.
        assert_eq!(got.as_deref(), Some("Reading the file."));
    }

    #[test]
    fn claude_multi_text_blocks_concatenates_and_picks_last_line() {
        let got = read_last_assistant_line_in(
            AgentKind::ClaudeCode,
            &claude_root(),
            "cafe1234-0000-0000-0000-000000000004",
            &mut None,
        );
        assert_eq!(got.as_deref(), Some("Second block line D."));
    }

    #[test]
    fn claude_empty_transcript_returns_none() {
        let got = read_last_assistant_line_in(
            AgentKind::ClaudeCode,
            &claude_root(),
            "cafe1234-0000-0000-0000-000000000005",
            &mut None,
        );
        assert!(got.is_none());
    }

    #[test]
    fn claude_malformed_lines_are_skipped() {
        let got = read_last_assistant_line_in(
            AgentKind::ClaudeCode,
            &claude_root(),
            "cafe1234-0000-0000-0000-000000000006",
            &mut None,
        );
        assert_eq!(got.as_deref(), Some("Malformed-resistant answer."));
    }

    #[test]
    fn claude_trailing_code_fence_is_stripped() {
        let got = read_last_assistant_line_in(
            AgentKind::ClaudeCode,
            &claude_root(),
            "cafe1234-0000-0000-0000-000000000007",
            &mut None,
        );
        // Last non-empty, non-fence line is the code content before the fence.
        assert_eq!(got.as_deref(), Some("let x = 1;"));
    }

    #[test]
    fn claude_cached_path_is_populated_on_success() {
        let mut cache = None;
        let _ = read_last_assistant_line_in(
            AgentKind::ClaudeCode,
            &claude_root(),
            "cafe1234-0000-0000-0000-000000000002",
            &mut cache,
        );
        assert!(cache.is_some());
        // Second call hits the cache — works even if the filesystem search would fail.
        let still = read_last_assistant_line_in(
            AgentKind::ClaudeCode,
            &claude_root().join("does_not_exist"),
            "ignored",
            &mut cache,
        );
        assert_eq!(still.as_deref(), Some("Starting now."));
    }

    fn codex_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transcripts/codex")
    }

    #[test]
    fn codex_path_found_by_recursive_walk() {
        let got = resolve_codex_path_in(&codex_root(), "codex0001-0000-0000-0000-000000000001");
        assert!(got.is_some(), "expected path resolution to succeed");
        let p = got.unwrap();
        assert!(p
            .to_string_lossy()
            .ends_with("codex0001-0000-0000-0000-000000000001.jsonl"));
    }

    #[test]
    fn codex_path_none_for_unknown_session() {
        let got = resolve_codex_path_in(&codex_root(), "nope");
        assert!(got.is_none());
    }

    #[test]
    fn codex_ends_with_text_returns_last_non_empty_line() {
        let got = read_last_assistant_line_in(
            AgentKind::Codex,
            &codex_root(),
            "codex0002-0000-0000-0000-000000000002",
            &mut None,
        );
        assert_eq!(got.as_deref(), Some("All set."));
    }

    #[test]
    fn codex_empty_returns_none() {
        let got = read_last_assistant_line_in(
            AgentKind::Codex,
            &codex_root(),
            "codex0003-0000-0000-0000-000000000003",
            &mut None,
        );
        assert!(got.is_none());
    }

    #[test]
    fn codex_malformed_lines_are_skipped() {
        let got = read_last_assistant_line_in(
            AgentKind::Codex,
            &codex_root(),
            "codex0004-0000-0000-0000-000000000004",
            &mut None,
        );
        assert_eq!(got.as_deref(), Some("Survived malformed lines."));
    }

    /// A prompt, a thought, and a question left hanging — the shape a daemon
    /// restart used to turn into "idle".
    const PENDING_QUESTION: &str = r#"{"type":"file-history-snapshot","sessionId":"sess-1"}
{"type":"user","sessionId":"sess-1","message":{"role":"user","content":"make me a ticket"}}
{"type":"assistant","sessionId":"sess-1","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"}]}}
{"type":"assistant","sessionId":"sess-1","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"AskUserQuestion","input":{"questions":[{"question":"Quelle priorité ?","header":"Priorité"}]}}]}}
{"type":"attachment","sessionId":"sess-1"}
{"type":"system","sessionId":"sess-1","content":"hook ran"}
"#;

    #[test]
    fn pending_question_waits_on_the_user() {
        let got = reduce_claude(PENDING_QUESTION).unwrap();
        assert_eq!(got.session_id, "sess-1");
        assert_eq!(got.status, SessionStatus::WaitingApproval);
        assert_eq!(got.current_tool.as_deref(), Some("AskUserQuestion"));
        assert_eq!(got.tool_detail.as_deref(), Some("Quelle priorité ?"));
    }

    #[test]
    fn answered_question_hands_the_turn_back() {
        let answered = format!(
            "{PENDING_QUESTION}{}\n",
            r#"{"type":"user","sessionId":"sess-1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1"}]}}"#
        );
        let got = reduce_claude(&answered).unwrap();
        assert_eq!(got.status, SessionStatus::Thinking);
        assert!(got.current_tool.is_none());
    }

    #[test]
    fn pending_tool_is_executing_with_its_detail() {
        let jsonl = r#"{"type":"user","sessionId":"s","message":{"role":"user","content":"build it"}}
{"type":"assistant","sessionId":"s","message":{"role":"assistant","content":[{"type":"text","text":"Running the suite."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test --all\n"}}]}}
"#;
        let got = reduce_claude(jsonl).unwrap();
        assert_eq!(got.status, SessionStatus::Executing);
        assert_eq!(got.current_tool.as_deref(), Some("Bash"));
        assert_eq!(got.tool_detail.as_deref(), Some("cargo test --all"));
    }

    #[test]
    fn parallel_calls_wait_for_the_last_result() {
        let both = r#"{"type":"assistant","sessionId":"s","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/a"}},{"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"/b"}}]}}
{"type":"user","sessionId":"s","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1"}]}}
"#;
        assert_eq!(
            reduce_claude(both).unwrap().status,
            SessionStatus::Executing,
            "one result in, one still outstanding"
        );
        let all = format!(
            "{both}{}\n",
            r#"{"type":"user","sessionId":"s","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2"}]}}"#
        );
        assert_eq!(reduce_claude(&all).unwrap().status, SessionStatus::Thinking);
    }

    #[test]
    fn closing_text_is_idle() {
        let jsonl = r#"{"type":"user","sessionId":"s","message":{"role":"user","content":"go"}}
{"type":"assistant","sessionId":"s","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","sessionId":"s","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1"}]}}
{"type":"assistant","sessionId":"s","message":{"role":"assistant","content":[{"type":"text","text":"Done."}]}}
"#;
        let got = reduce_claude(jsonl).unwrap();
        assert_eq!(got.status, SessionStatus::Idle);
        assert!(got.current_tool.is_none());
    }

    #[test]
    fn sidechain_and_meta_records_are_ignored() {
        let jsonl = format!(
            "{PENDING_QUESTION}{}\n{}\n",
            r#"{"type":"user","sessionId":"sess-1","isMeta":true,"message":{"role":"user","content":"<system-reminder>"}}"#,
            r#"{"type":"assistant","sessionId":"sess-1","isSidechain":true,"message":{"role":"assistant","content":[{"type":"text","text":"sub-agent report"}]}}"#
        );
        assert_eq!(
            reduce_claude(&jsonl).unwrap().status,
            SessionStatus::WaitingApproval
        );
    }

    /// A sub-agent mid-tool-call, every record marked `isSidechain` the way
    /// Claude Code writes agent-*.jsonl files, and with no `sessionId` — the
    /// two traits that make `reduce_claude` itself unusable on them.
    const RUNNING_SUBAGENT: &str = r#"{"parentUuid":null,"isSidechain":true,"agentId":"a1","type":"user","message":{"role":"user","content":"audit the module"}}
{"isSidechain":true,"agentId":"a1","type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"rg todo"}}]}}
"#;

    const FINISHED_SUBAGENT: &str = r#"{"parentUuid":null,"isSidechain":true,"agentId":"a2","type":"user","message":{"role":"user","content":"audit the module"}}
{"isSidechain":true,"agentId":"a2","type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"All clear."}]}}
"#;

    #[test]
    fn outstanding_subagents_counts_only_midturn_transcripts() {
        let root = std::env::temp_dir().join(format!("vibewatch-subcount-{}", std::process::id()));
        let dir = root.join("sess-42/subagents");
        std::fs::create_dir_all(&dir).unwrap();
        let transcript = root.join("sess-42.jsonl");
        std::fs::write(&transcript, "").unwrap();
        std::fs::write(dir.join("agent-a1.jsonl"), RUNNING_SUBAGENT).unwrap();
        std::fs::write(dir.join("agent-a2.jsonl"), FINISHED_SUBAGENT).unwrap();
        // Sidecar metadata next to the transcripts must not be read as one.
        std::fs::write(dir.join("agent-a1.meta.json"), "{}").unwrap();
        assert_eq!(
            count_outstanding_subagents(&transcript, std::time::UNIX_EPOCH),
            1,
            "the running agent counts, the finished one and the sidecar don't"
        );
        // A resumed session keeps its directory, so files untouched since
        // before the floor belong to an earlier process and are not counted
        // even when they ended mid-turn.
        let future = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
        assert_eq!(count_outstanding_subagents(&transcript, future), 0);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn outstanding_subagents_without_a_dir_is_zero() {
        assert_eq!(
            count_outstanding_subagents(
                Path::new("/nonexistent/sess.jsonl"),
                std::time::UNIX_EPOCH
            ),
            0
        );
    }

    #[test]
    fn prompt_with_no_reply_yet_is_thinking() {
        let jsonl = r#"{"type":"user","sessionId":"s","message":{"role":"user","content":[{"type":"text","text":"why is it idle?"}]}}"#;
        assert_eq!(
            reduce_claude(jsonl).unwrap().status,
            SessionStatus::Thinking
        );
    }

    #[test]
    fn transcript_without_a_session_id_is_not_a_snapshot() {
        assert!(reduce_claude(r#"{"type":"summary","summary":"nothing useful"}"#).is_none());
    }

    #[test]
    fn malformed_lines_do_not_derail_the_reduction() {
        let jsonl = format!("not json\n{PENDING_QUESTION}\n{{\"broken\":\n");
        assert_eq!(
            reduce_claude(&jsonl).unwrap().status,
            SessionStatus::WaitingApproval
        );
    }

    #[test]
    fn project_dir_name_dashes_everything_but_alphanumerics() {
        assert_eq!(
            project_dir_name(Path::new("/home/moinax/Projects/o27/cppb.preview")),
            "-home-moinax-Projects-o27-cppb-preview"
        );
        assert_eq!(
            project_dir_name(Path::new("/home/moinax/.t3/worktrees/x")),
            "-home-moinax--t3-worktrees-x"
        );
    }

    #[test]
    fn project_dir_resolves_through_a_differing_mangling() {
        let tmp = tempfile::tempdir().unwrap();
        // Claude Code kept the underscore where our rule dashes it.
        let actual = tmp.path().join("-home-moinax-my_project");
        std::fs::create_dir(&actual).unwrap();
        let got = project_dir_for_cwd(tmp.path(), Path::new("/home/moinax/my_project"));
        assert_eq!(got.as_deref(), Some(actual.as_path()));
    }

    #[test]
    fn transcript_search_takes_the_newest_and_skips_the_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("-repo");
        std::fs::create_dir(&dir).unwrap();
        let old = dir.join("old.jsonl");
        let live = dir.join("live.jsonl");
        std::fs::write(&old, "{}\n").unwrap();
        std::fs::write(&live, "{}\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();
        let floor = std::fs::metadata(&live).unwrap().modified().unwrap();
        // Backdate the stale one well behind the floor.
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(floor - std::time::Duration::from_secs(3600)),
            )
            .unwrap();
        let got = find_claude_transcript_for_cwd(
            tmp.path(),
            Path::new("/repo"),
            floor - std::time::Duration::from_secs(5),
        );
        assert_eq!(got.as_deref(), Some(live.as_path()));
    }
}
