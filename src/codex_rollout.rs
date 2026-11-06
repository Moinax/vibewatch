//! Read Codex CLI rollout JSONL files and reduce them to the small live-state
//! model used by vibewatch.
//!
//! Claude Code calls hooks for these transitions. Codex CLI does not expose an
//! equivalent hook surface, but it persists the same information to
//! `~/.codex/sessions/**/rollout-*.jsonl`. The scanner polls the file belonging
//! to each live Codex process, which also covers tools run by Codex subagents:
//! collaboration calls are ordinary tool-call items in the parent rollout.

use crate::session::SessionStatus;
use serde_json::Value;
use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// Resolve the rollout currently held open by a live Codex process.
///
/// A Codex process can rotate to a new rollout without changing PID (resume,
/// compaction, or a new thread). Looking up by cwd cannot distinguish two
/// Codex processes in the same repository and, once cached, misses that
/// rotation entirely. The writer's open file descriptor is unambiguous.
/// Where Codex keeps its rollouts, `~/.codex/sessions`. Named here because
/// this module is what knows the layout underneath it.
pub fn sessions_root() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".codex").join("sessions"))
}

pub fn find_open_for_pid(pid: u32) -> Option<PathBuf> {
    find_open_in(Path::new("/proc"), pid)
}

fn find_open_in(proc_root: &Path, pid: u32) -> Option<PathBuf> {
    let fd_dir = proc_root.join(pid.to_string()).join("fd");
    std::fs::read_dir(fd_dir)
        .ok()?
        .flatten()
        .filter_map(|entry| std::fs::read_link(entry.path()).ok())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
                .unwrap_or(false)
                && path.components().any(|part| part.as_os_str() == "sessions")
        })
        .max_by_key(|path| {
            std::fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::UNIX_EPOCH)
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolloutSnapshot {
    pub session_id: String,
    pub cwd: Option<String>,
    pub status: SessionStatus,
    pub current_tool: Option<String>,
    pub tool_detail: Option<String>,
    pub last_tool: Option<String>,
    pub last_prompt: Option<String>,
}

pub fn find_latest_for_cwd(
    root: &Path,
    cwd: &Path,
    not_before: std::time::SystemTime,
) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    while let Some(dir) = stack.pop() {
        // One unreadable directory costs only itself: a `?` here would throw away
        // the rest of the tree, and any match already in hand.
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // `file_type` rides along with the directory read on Linux, where
            // `path.is_dir()` is a second stat per file on top of the mtime's.
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            // The mtime first, then the first line and nothing past it. This runs
            // on every scan tick for as long as a live Codex process has no rollout
            // open — a `codex app-server` probe never gets one — over a tree that
            // only grows: reading each file whole cost gigabytes per tick to look
            // at one line of it, and charged all of them to the daemon's cgroup.
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            if modified < not_before {
                continue;
            }
            let mut line = String::new();
            if std::fs::File::open(&path)
                .and_then(|file| BufReader::new(file).read_line(&mut line))
                .is_err()
            {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let payload = &value["payload"];
            if payload.get("cwd").and_then(Value::as_str).map(Path::new) != Some(cwd) {
                continue;
            }
            if best
                .as_ref()
                .map(|(time, _)| modified > *time)
                .unwrap_or(true)
            {
                best = Some((modified, path));
            }
        }
    }
    best.map(|(_, path)| path)
}

pub fn parse_file(path: &Path) -> Option<RolloutSnapshot> {
    parse(&crate::transcript::head_and_tail(path)?)
}

pub fn parse(content: &str) -> Option<RolloutSnapshot> {
    let mut session_id = None;
    let mut cwd = None;
    let mut status = SessionStatus::Idle;
    let mut current_tool = None;
    let mut tool_detail = None;
    let mut last_tool = None;
    let mut last_prompt = None;
    let mut pending_calls = HashSet::new();

    for line in content.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let payload = &record["payload"];
        match record.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                session_id = payload
                    .get("session_id")
                    .or_else(|| payload.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                cwd = payload
                    .get("cwd")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            Some("event_msg") => match payload.get("type").and_then(Value::as_str) {
                Some("task_started") => {
                    status = SessionStatus::Thinking;
                    current_tool = None;
                    tool_detail = None;
                    pending_calls.clear();
                }
                Some("task_complete") | Some("turn_aborted") => {
                    status = SessionStatus::Idle;
                    current_tool = None;
                    tool_detail = None;
                    pending_calls.clear();
                }
                Some("user_message") => {
                    last_prompt = payload
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    status = SessionStatus::Thinking;
                }
                _ => {}
            },
            Some("response_item") => {
                let kind = payload.get("type").and_then(Value::as_str).unwrap_or("");
                if matches!(kind, "function_call" | "custom_tool_call") {
                    let name = payload
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool");
                    let call_id = payload.get("call_id").and_then(Value::as_str).unwrap_or("");
                    if !call_id.is_empty() {
                        pending_calls.insert(call_id.to_owned());
                    }
                    current_tool = Some(name.to_owned());
                    tool_detail = extract_detail(payload);
                    status = SessionStatus::Executing;
                } else if matches!(kind, "function_call_output" | "custom_tool_call_output") {
                    if let Some(call_id) = payload.get("call_id").and_then(Value::as_str) {
                        pending_calls.remove(call_id);
                    }
                    if pending_calls.is_empty() {
                        if let Some(tool) = current_tool.take() {
                            last_tool = Some(tool);
                        }
                        tool_detail = None;
                        status = SessionStatus::Thinking;
                    }
                }
            }
            _ => {}
        }
    }

    Some(RolloutSnapshot {
        session_id: session_id?,
        cwd,
        status,
        current_tool,
        tool_detail,
        last_tool,
        last_prompt,
    })
}

/// Codex nests the tool input one level down and sometimes as a JSON *string*;
/// once unwrapped it is the same object shape Claude Code writes, so the
/// key probe and truncation are shared.
fn extract_detail(payload: &Value) -> Option<String> {
    let raw = payload.get("arguments").or_else(|| payload.get("input"))?;
    let value = match raw {
        Value::String(s) => serde_json::from_str::<Value>(s).ok(),
        other => Some(other.clone()),
    }?;
    crate::transcript::tool_detail_from_input(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn open_fd_identifies_the_rollout_for_one_pid() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join(".codex/sessions/2026/07/30");
        let fd_dir = tmp.path().join("proc/42/fd");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::create_dir_all(&fd_dir).unwrap();
        let rollout = sessions.join("rollout-current.jsonl");
        let unrelated = tmp.path().join("notes.jsonl");
        std::fs::write(&rollout, "{}\n").unwrap();
        std::fs::write(&unrelated, "{}\n").unwrap();
        symlink(&unrelated, fd_dir.join("3")).unwrap();
        symlink(&rollout, fd_dir.join("4")).unwrap();

        assert_eq!(find_open_in(&tmp.path().join("proc"), 42), Some(rollout));
    }

    /// Only the first line is any of this search's business: a tail that would
    /// fail a whole-file UTF-8 read must not hide the rollout, and one untouched
    /// since before the process started must not be pinned on it.
    #[test]
    fn latest_for_cwd_reads_the_first_line_of_recent_rollouts() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = Path::new("/repo");
        let meta = b"{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/repo\"}}\n";
        let write = |name: &str| {
            let path = tmp.path().join(name);
            let mut bytes = meta.to_vec();
            bytes.extend_from_slice(b"\xff\xfe not utf-8\n");
            std::fs::write(&path, bytes).unwrap();
            path
        };
        let floor = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        let old = write("old.jsonl");
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(floor - std::time::Duration::from_secs(60))
            .unwrap();
        assert_eq!(find_latest_for_cwd(tmp.path(), cwd, floor), None);

        let recent = write("recent.jsonl");
        assert_eq!(find_latest_for_cwd(tmp.path(), cwd, floor), Some(recent));
        assert_eq!(
            find_latest_for_cwd(tmp.path(), Path::new("/x"), floor),
            None
        );
    }

    /// The root's files are walked before the subdirectory it pushes, so the
    /// match is already in hand when the unreadable directory comes up. Passes
    /// vacuously as root, which ignores the mode.
    #[cfg(unix)]
    #[test]
    fn latest_for_cwd_survives_an_unreadable_directory() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let rollout = tmp.path().join("rollout.jsonl");
        std::fs::write(
            &rollout,
            "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/repo\"}}\n",
        )
        .unwrap();
        let locked = tmp.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let found = find_latest_for_cwd(tmp.path(), Path::new("/repo"), std::time::UNIX_EPOCH);

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(found, Some(rollout));
    }

    #[test]
    fn reduces_tool_lifecycle() {
        let jsonl = r#"{"type":"session_meta","payload":{"id":"thread-1","cwd":"/repo"}}
{"type":"event_msg","payload":{"type":"task_started"}}
{"type":"event_msg","payload":{"type":"user_message","message":"fix it"}}
{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"c1","input":"{\"cmd\":\"cargo test\"}"}}
"#;
        let got = parse(jsonl).unwrap();
        assert_eq!(got.session_id, "thread-1");
        assert_eq!(got.status, SessionStatus::Executing);
        assert_eq!(got.current_tool.as_deref(), Some("exec"));
        assert_eq!(got.tool_detail.as_deref(), Some("cargo test"));
        assert_eq!(got.last_prompt.as_deref(), Some("fix it"));
    }

    #[test]
    fn completed_call_thinks_until_task_complete() {
        let jsonl = r#"{"type":"session_meta","payload":{"id":"thread-1"}}
{"type":"event_msg","payload":{"type":"task_started"}}
{"type":"response_item","payload":{"type":"function_call","name":"spawn_agent","call_id":"c1","arguments":"{\"task_name\":\"review\"}"}}
{"type":"response_item","payload":{"type":"function_call_output","call_id":"c1"}}
"#;
        let got = parse(jsonl).unwrap();
        assert_eq!(got.status, SessionStatus::Thinking);
        assert_eq!(got.last_tool.as_deref(), Some("spawn_agent"));
    }
}
