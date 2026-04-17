use anyhow::{bail, Context};
use serde::Deserialize;
use std::io::Read;

use crate::config::Config;
use crate::ipc::{send_event, InboundEvent};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Outcome of a blocking permission-request round-trip with the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny,
    Ask,
}

impl PermissionDecision {
    fn as_claude_str(self) -> &'static str {
        match self {
            PermissionDecision::Allow => "allow",
            PermissionDecision::Deny => "deny",
            PermissionDecision::Ask => "ask",
        }
    }
}

/// Connect to the daemon, send a `PermissionRequest` event, keep the stream
/// open and block reading one JSON decision line. `timeout` bounds the whole
/// exchange. Returns the parsed decision. Connection failure is returned as
/// `Err` so the caller can translate to `Ask`.
pub async fn send_permission_request(
    socket_path: &std::path::Path,
    event: &InboundEvent,
    timeout: std::time::Duration,
) -> anyhow::Result<PermissionDecision> {
    use anyhow::Context;
    let mut stream = UnixStream::connect(socket_path)
        .await
        .context("connect to vibewatch daemon")?;

    let mut json = serde_json::to_string(event)?;
    json.push('\n');
    stream.write_all(json.as_bytes()).await?;
    stream.flush().await?;

    let (read_half, _write_half) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(read_half);
    let mut line = String::new();

    let read_fut = reader.read_line(&mut line);
    match tokio::time::timeout(timeout, read_fut).await {
        Ok(Ok(n)) if n > 0 => {
            let v: serde_json::Value = serde_json::from_str(line.trim())?;
            let approved = v.get("approved").and_then(|x| x.as_bool()).unwrap_or(false);
            Ok(if approved {
                PermissionDecision::Allow
            } else {
                PermissionDecision::Deny
            })
        }
        Ok(Ok(_)) => Ok(PermissionDecision::Ask), // EOF — treat as fallback
        Ok(Err(e)) => Err(anyhow::anyhow!("read error: {e}")),
        Err(_) => Ok(PermissionDecision::Ask),    // timeout
    }
}

/// Claude Code hook JSON envelope (received on stdin)
#[derive(Debug, Deserialize)]
pub struct ClaudeCodeHook {
    pub session_id: String,
    pub hook_event_name: String,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<serde_json::Value>,
    #[serde(default)]
    pub tool_response: Option<serde_json::Value>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub transcript_path: Option<String>,
}

/// Codex hook JSON envelope
#[derive(Debug, Deserialize)]
pub struct CodexHook {
    pub session_id: String,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<serde_json::Value>,
    #[serde(default)]
    pub tool_response: Option<serde_json::Value>,
}

/// Read stdin, parse hook JSON based on agent, and send an IPC event to the daemon.
pub async fn handle_notify(event_type: &str, agent: &str) -> anyhow::Result<()> {
    let mut stdin_buf = String::new();
    std::io::stdin()
        .read_to_string(&mut stdin_buf)
        .context("failed to read stdin")?;

    let event = match agent {
        "claude-code" => parse_claude_code(&stdin_buf, event_type)?,
        "codex" => parse_codex(&stdin_buf, event_type)?,
        other => bail!("unknown agent: {}", other),
    };

    let config = Config::load()?;
    let socket_path = config.socket_path();

    if agent == "claude-code" && event_type == "permission-request" {
        let decision = match send_permission_request(
            &socket_path,
            &event,
            std::time::Duration::from_secs(580),
        )
        .await
        {
            Ok(d) => d,
            Err(e) => {
                eprintln!("vibewatch: permission-request fallback ask ({e})");
                PermissionDecision::Ask
            }
        };
        let out = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "permissionDecision": decision.as_claude_str(),
                "permissionDecisionReason": "via vibewatch widget",
            }
        });
        println!("{}", serde_json::to_string(&out)?);
        return Ok(());
    }

    send_event(&socket_path, &event).await?;
    Ok(())
}

/// Get the parent PID (the claude process that spawned this hook).
fn parent_pid() -> u32 {
    std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|stat| {
            let after_paren = stat.rfind(')')?;
            let rest = &stat[after_paren + 2..];
            let fields: Vec<&str> = rest.split_whitespace().collect();
            fields.get(1)?.parse::<u32>().ok()
        })
        .unwrap_or_else(std::process::id)
}

/// Map a Claude Code hook JSON payload to an IPC `InboundEvent`.
pub fn parse_claude_code(stdin: &str, event_type: &str) -> anyhow::Result<InboundEvent> {
    let hook: ClaudeCodeHook =
        serde_json::from_str(stdin).context("failed to parse Claude Code hook JSON")?;

    match event_type {
        "session-start" => {
            let session_name = hook.transcript_path.as_deref()
                .and_then(read_session_name);
            Ok(InboundEvent::SessionStart {
                agent: "claude_code".to_string(),
                session_id: hook.session_id,
                pid: parent_pid(),
                cwd: hook.cwd,
                session_name,
            })
        }
        "pre-tool-use" => Ok(InboundEvent::PreToolUse {
            session_id: hook.session_id,
            tool: hook.tool_name.unwrap_or_default(),
            detail: extract_tool_detail(&hook.tool_input),
            pid: Some(parent_pid()),
        }),
        "post-tool-use" => {
            let success = hook
                .tool_response
                .as_ref()
                .and_then(|v| v.get("success"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Ok(InboundEvent::PostToolUse {
                session_id: hook.session_id,
                tool: hook.tool_name.unwrap_or_default(),
                success,
                pid: Some(parent_pid()),
            })
        }
        "user-prompt-submit" => {
            let prompt = hook.prompt.map(|p| {
                if p.len() > 100 {
                    format!("{}...", &p[..97])
                } else {
                    p
                }
            });
            Ok(InboundEvent::UserPromptSubmit {
                session_id: hook.session_id,
                prompt,
                pid: Some(parent_pid()),
            })
        }
        "permission-request" => {
            let pid = parent_pid();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let request_id = format!("{}-{}-{}", hook.session_id, pid, nanos);
            Ok(InboundEvent::PermissionRequest {
                session_id: hook.session_id,
                request_id: Some(request_id),
                tool: hook.tool_name,
                detail: extract_tool_detail(&hook.tool_input),
                pid: Some(pid),
            })
        }
        "permission-denied" => Ok(InboundEvent::PermissionDenied {
            session_id: hook.session_id,
            pid: Some(parent_pid()),
        }),
        "stop" => Ok(InboundEvent::Stop {
            session_id: hook.session_id,
            pid: Some(parent_pid()),
        }),
        other => bail!("unknown event type: {}", other),
    }
}

/// Map a Codex hook JSON payload to an IPC `InboundEvent`.
pub fn parse_codex(stdin: &str, event_type: &str) -> anyhow::Result<InboundEvent> {
    let hook: CodexHook =
        serde_json::from_str(stdin).context("failed to parse Codex hook JSON")?;

    match event_type {
        "session-start" => Ok(InboundEvent::SessionStart {
            agent: "codex".to_string(),
            session_id: hook.session_id,
            pid: parent_pid(),
            cwd: None,
            session_name: None,
        }),
        "pre-tool-use" => Ok(InboundEvent::PreToolUse {
            session_id: hook.session_id,
            tool: hook.tool_name.unwrap_or_default(),
            detail: extract_tool_detail(&hook.tool_input),
            pid: Some(parent_pid()),
        }),
        "post-tool-use" => {
            let success = hook
                .tool_response
                .as_ref()
                .and_then(|v| v.get("success"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Ok(InboundEvent::PostToolUse {
                session_id: hook.session_id,
                tool: hook.tool_name.unwrap_or_default(),
                success,
                pid: Some(parent_pid()),
            })
        }
        "stop" => Ok(InboundEvent::Stop {
            session_id: hook.session_id,
            pid: Some(parent_pid()),
        }),
        other => bail!("unknown event type: {}", other),
    }
}

/// Read the session name from a Claude Code transcript file.
/// Scans from the end for efficiency since title entries appear throughout.
fn read_session_name(transcript_path: &str) -> Option<String> {
    // Read the whole file and scan backwards for the last title entry
    let content = std::fs::read_to_string(transcript_path).ok()?;
    for line in content.lines().rev() {
        if line.contains("\"custom-title\"") {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(title) = val.get("customTitle").and_then(|v| v.as_str()) {
                    return Some(title.to_string());
                }
            }
        }
        if line.contains("\"agent-name\"") {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(name) = val.get("agentName").and_then(|v| v.as_str()) {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

/// Extract a human-readable detail from tool_input JSON.
///
/// Looks for "command" or "file_path" fields. Truncates to 80 characters
/// (77 chars + "...") if the value is longer.
pub fn extract_tool_detail(tool_input: &Option<serde_json::Value>) -> Option<String> {
    let input = tool_input.as_ref()?;

    let value = input
        .get("command")
        .or_else(|| input.get("file_path"))
        .and_then(|v| v.as_str())?;

    if value.len() > 80 {
        Some(format!("{}...", &value[..77]))
    } else {
        Some(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_claude_code_session_start() {
        let json = r#"{"session_id":"abc123","hook_event_name":"session-start","cwd":"/home/user/project"}"#;
        let event = parse_claude_code(json, "session-start").unwrap();
        match event {
            InboundEvent::SessionStart {
                agent,
                session_id,
                pid: _,
                cwd,
                session_name,
            } => {
                assert_eq!(agent, "claude_code");
                assert_eq!(session_id, "abc123");
                assert_eq!(cwd.as_deref(), Some("/home/user/project"));
                assert!(session_name.is_none()); // no transcript path in test
            }
            _ => panic!("expected SessionStart"),
        }
    }

    #[test]
    fn test_parse_claude_code_pre_tool_use() {
        let json = r#"{"session_id":"abc123","hook_event_name":"pre-tool-use","tool_name":"Bash","tool_input":{"command":"npm test"}}"#;
        let event = parse_claude_code(json, "pre-tool-use").unwrap();
        match event {
            InboundEvent::PreToolUse {
                session_id,
                tool,
                detail,
                pid: _,
            } => {
                assert_eq!(session_id, "abc123");
                assert_eq!(tool, "Bash");
                assert_eq!(detail.unwrap(), "npm test");
            }
            _ => panic!("expected PreToolUse"),
        }
    }

    #[test]
    fn test_parse_claude_code_post_tool_use() {
        let json = r#"{"session_id":"abc123","hook_event_name":"post-tool-use","tool_name":"Bash","tool_response":{"success":true}}"#;
        let event = parse_claude_code(json, "post-tool-use").unwrap();
        match event {
            InboundEvent::PostToolUse {
                session_id,
                tool,
                success,
                pid: _,
            } => {
                assert_eq!(session_id, "abc123");
                assert_eq!(tool, "Bash");
                assert!(success);
            }
            _ => panic!("expected PostToolUse"),
        }
    }

    #[test]
    fn test_parse_codex_pre_tool_use() {
        let json = r#"{"session_id":"codex-1","tool_name":"shell","tool_input":{"command":"cargo build"}}"#;
        let event = parse_codex(json, "pre-tool-use").unwrap();
        match event {
            InboundEvent::PreToolUse {
                session_id,
                tool,
                detail,
                pid: _,
            } => {
                assert_eq!(session_id, "codex-1");
                assert_eq!(tool, "shell");
                assert_eq!(detail.unwrap(), "cargo build");
            }
            _ => panic!("expected PreToolUse"),
        }
    }

    #[test]
    fn test_extract_tool_detail_command() {
        let input = Some(serde_json::json!({"command": "npm test"}));
        assert_eq!(extract_tool_detail(&input).unwrap(), "npm test");
    }

    #[test]
    fn test_extract_tool_detail_file_path() {
        let input = Some(serde_json::json!({"file_path": "src/main.rs"}));
        assert_eq!(extract_tool_detail(&input).unwrap(), "src/main.rs");
    }

    #[test]
    fn test_extract_tool_detail_truncates_long_commands() {
        let long_cmd = "a".repeat(100);
        let input = Some(serde_json::json!({"command": long_cmd}));
        let result = extract_tool_detail(&input).unwrap();
        assert_eq!(result.len(), 80);
        assert!(result.ends_with("..."));
        assert_eq!(&result[..77], &"a".repeat(77));
    }

    #[test]
    fn test_extract_tool_detail_none() {
        assert!(extract_tool_detail(&None).is_none());
    }

    #[test]
    fn test_unknown_event_type_errors() {
        let json = r#"{"session_id":"abc123","hook_event_name":"unknown"}"#;
        let result = parse_claude_code(json, "bogus");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown event type"));
    }

    #[test]
    fn test_parse_claude_code_permission_request_sets_all_fields() {
        let json = r#"{"session_id":"abc123","hook_event_name":"permission-request","tool_name":"Bash","tool_input":{"command":"rm -rf /tmp"}}"#;
        let event = parse_claude_code(json, "permission-request").unwrap();
        match event {
            InboundEvent::PermissionRequest {
                session_id,
                request_id,
                tool,
                detail,
                pid,
            } => {
                assert_eq!(session_id, "abc123");
                let rid = request_id.expect("request_id must be set by hook");
                assert!(rid.contains("abc123"), "request_id should contain session_id, got {:?}", rid);
                assert_eq!(tool.as_deref(), Some("Bash"));
                assert_eq!(detail.as_deref(), Some("rm -rf /tmp"));
                assert!(pid.is_some());
            }
            _ => panic!("expected PermissionRequest"),
        }
    }

    #[tokio::test]
    async fn send_permission_request_reads_decision_line() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        use tokio::net::{UnixListener, UnixStream};

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("v.sock");
        let listener = UnixListener::bind(&path).unwrap();

        let server_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = stream.split();
            let mut reader = tokio::io::BufReader::new(read_half);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            assert!(line.contains("\"event\":\"permission_request\""));
            write_half
                .write_all(b"{\"approved\":true}\n")
                .await
                .unwrap();
            write_half.flush().await.unwrap();
            let mut discard = String::new();
            let _ = reader.read_line(&mut discard).await;
        });

        let event = InboundEvent::PermissionRequest {
            session_id: "s1".into(),
            request_id: Some("r1".into()),
            tool: Some("Bash".into()),
            detail: Some("ls".into()),
            pid: Some(42),
        };
        let decision = send_permission_request(&path, &event, std::time::Duration::from_secs(2))
            .await
            .expect("round-trip succeeds");
        assert_eq!(decision, PermissionDecision::Allow);

        let _ = server_task.await;
    }

    #[tokio::test]
    async fn send_permission_request_errors_when_daemon_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("v.sock");
        // Don't bind a listener — connect will fail, function returns Err.
        let event = InboundEvent::PermissionRequest {
            session_id: "s1".into(),
            request_id: Some("r1".into()),
            tool: Some("Bash".into()),
            detail: None,
            pid: None,
        };
        let result = send_permission_request(&path, &event, std::time::Duration::from_millis(100)).await;
        assert!(result.is_err(), "missing daemon socket should produce an error");
    }
}
