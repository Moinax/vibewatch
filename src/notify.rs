use anyhow::{bail, Context};
use serde::Deserialize;
use std::io::Read;

use crate::config::Config;
use crate::ipc::{send_event, InboundEvent};

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
    send_event(&socket_path, &event).await?;

    Ok(())
}

/// Map a Claude Code hook JSON payload to an IPC `InboundEvent`.
pub fn parse_claude_code(stdin: &str, event_type: &str) -> anyhow::Result<InboundEvent> {
    let hook: ClaudeCodeHook =
        serde_json::from_str(stdin).context("failed to parse Claude Code hook JSON")?;

    match event_type {
        "session-start" => Ok(InboundEvent::SessionStart {
            agent: "claude_code".to_string(),
            session_id: hook.session_id,
            pid: std::process::id(),
            cwd: hook.cwd,
        }),
        "pre-tool-use" => Ok(InboundEvent::PreToolUse {
            session_id: hook.session_id,
            tool: hook.tool_name.unwrap_or_default(),
            detail: extract_tool_detail(&hook.tool_input),
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
            })
        }
        "stop" => Ok(InboundEvent::Stop {
            session_id: hook.session_id,
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
            pid: std::process::id(),
            cwd: None, // Codex hook doesn't provide cwd
        }),
        "pre-tool-use" => Ok(InboundEvent::PreToolUse {
            session_id: hook.session_id,
            tool: hook.tool_name.unwrap_or_default(),
            detail: extract_tool_detail(&hook.tool_input),
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
            })
        }
        "stop" => Ok(InboundEvent::Stop {
            session_id: hook.session_id,
        }),
        other => bail!("unknown event type: {}", other),
    }
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
                pid,
                cwd,
            } => {
                assert_eq!(agent, "claude_code");
                assert_eq!(session_id, "abc123");
                assert_eq!(pid, std::process::id());
                assert_eq!(cwd.as_deref(), Some("/home/user/project"));
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
}
