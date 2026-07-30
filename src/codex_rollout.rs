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
use std::path::{Path, PathBuf};

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
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(first) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(line) = first.lines().next() else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let payload = &value["payload"];
            if payload.get("cwd").and_then(Value::as_str).map(Path::new) != Some(cwd) {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            if modified < not_before {
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
