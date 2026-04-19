use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// Inbound event from hooks — tagged enum
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum InboundEvent {
    SessionStart {
        agent: String,
        session_id: String,
        pid: u32,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        session_name: Option<String>,
    },
    PreToolUse {
        session_id: String,
        tool: String,
        #[serde(default)]
        detail: Option<String>,
        #[serde(default)]
        pid: Option<u32>,
    },
    PostToolUse {
        session_id: String,
        tool: String,
        #[serde(default)]
        success: bool,
        #[serde(default)]
        pid: Option<u32>,
    },
    UserPromptSubmit {
        session_id: String,
        #[serde(default)]
        prompt: Option<String>,
        #[serde(default)]
        pid: Option<u32>,
    },
    PermissionRequest {
        session_id: String,
        #[serde(default)]
        request_id: Option<String>,
        #[serde(default)]
        tool: Option<String>,
        #[serde(default)]
        detail: Option<String>,
        #[serde(default)]
        pid: Option<u32>,
        #[serde(default)]
        permission_suggestions: Vec<crate::session::PermissionSuggestion>,
        /// Button labels for tools whose UI is really a multiple-choice
        /// (currently just `AskUserQuestion` with a single non-multiSelect
        /// question). Empty for ordinary permission prompts.
        #[serde(default)]
        option_labels: Vec<String>,
    },
    PermissionDenied {
        session_id: String,
        #[serde(default)]
        pid: Option<u32>,
    },
    Stop {
        session_id: String,
        #[serde(default)]
        pid: Option<u32>,
    },
    GetStatus,
    /// Subscribe to status updates. The connection stays open; the daemon
    /// writes one JSON line per state change (prefixed with an immediate
    /// snapshot so the subscriber doesn't wait for the next transition).
    SubscribeStatus,
    TogglePanel,
    ApprovalDecision {
        request_id: String,
        choice_index: usize,
    },
}

/// Status response for Waybar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub text: String,
    pub class: String,
}

/// Unix socket IPC server.
pub struct IpcServer {
    listener: UnixListener,
    path: PathBuf,
}

impl IpcServer {
    /// Bind a new Unix socket server at the given path.
    ///
    /// Removes any stale socket file, creates parent directories if needed,
    /// and binds a `UnixListener`.
    pub fn bind(path: &Path) -> anyhow::Result<Self> {
        // Create parent directories
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Remove stale socket
        if path.exists() {
            std::fs::remove_file(path)?;
        }

        // Bind using std first, then convert to tokio
        let std_listener = std::os::unix::net::UnixListener::bind(path)?;
        std_listener.set_nonblocking(true)?;
        let listener = UnixListener::from_std(std_listener)?;

        Ok(Self {
            listener,
            path: path.to_path_buf(),
        })
    }

    /// Accept a new connection.
    pub async fn accept(&self) -> anyhow::Result<UnixStream> {
        let (stream, _addr) = self.listener.accept().await?;
        Ok(stream)
    }

    /// Returns the socket path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Read one JSON line from the stream and parse it as an `InboundEvent`.
pub async fn read_event<R: AsyncBufRead + Unpin>(
    stream: &mut R,
) -> anyhow::Result<InboundEvent> {
    let mut line = String::new();
    let n = stream.read_line(&mut line).await?;
    if n == 0 {
        anyhow::bail!("connection closed");
    }
    let event: InboundEvent = serde_json::from_str(line.trim())?;
    Ok(event)
}

/// Write a JSON value followed by a newline to the stream.
pub async fn write_json<T: Serialize>(
    stream: &mut UnixStream,
    value: &T,
) -> anyhow::Result<()> {
    let mut json = serde_json::to_string(value)?;
    json.push('\n');
    stream.write_all(json.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

/// Connect to the socket, send an event, and read back an optional response line.
pub async fn send_event(
    socket_path: &Path,
    event: &InboundEvent,
) -> anyhow::Result<Option<String>> {
    let mut stream = UnixStream::connect(socket_path).await?;
    write_json(&mut stream, event).await?;

    // Try to read a response line
    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    let n = reader.read_line(&mut response).await?;
    if n == 0 {
        Ok(None)
    } else {
        Ok(Some(response.trim().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_session_start() {
        let json = r#"{"event":"session_start","agent":"claude_code","session_id":"s1","pid":1234}"#;
        let event: InboundEvent = serde_json::from_str(json).unwrap();
        match event {
            InboundEvent::SessionStart {
                agent,
                session_id,
                pid,
                cwd,
                session_name,
            } => {
                assert_eq!(agent, "claude_code");
                assert_eq!(session_id, "s1");
                assert_eq!(pid, 1234);
                assert!(cwd.is_none());
                assert!(session_name.is_none());
            }
            _ => panic!("expected SessionStart"),
        }
    }

    #[test]
    fn test_parse_pre_tool_use() {
        let json = r#"{"event":"pre_tool_use","session_id":"s1","tool":"Read","detail":"src/main.rs"}"#;
        let event: InboundEvent = serde_json::from_str(json).unwrap();
        match event {
            InboundEvent::PreToolUse {
                session_id,
                tool,
                detail,
                pid: _,
            } => {
                assert_eq!(session_id, "s1");
                assert_eq!(tool, "Read");
                assert_eq!(detail.unwrap(), "src/main.rs");
            }
            _ => panic!("expected PreToolUse"),
        }
    }

    #[test]
    fn test_parse_stop() {
        let json = r#"{"event":"stop","session_id":"s1"}"#;
        let event: InboundEvent = serde_json::from_str(json).unwrap();
        match event {
            InboundEvent::Stop { session_id, pid: _ } => {
                assert_eq!(session_id, "s1");
            }
            _ => panic!("expected Stop"),
        }
    }

    #[test]
    fn test_serialize_status_response() {
        let response = StatusResponse {
            text: "1 active".into(),
            class: "active".into(),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"text\":\"1 active\""));
        assert!(json.contains("\"class\":\"active\""));
    }

    #[tokio::test]
    async fn test_server_bind_and_connect() {
        let tmp = tempfile::TempDir::new().unwrap();
        let socket_path = tmp.path().join("vibewatch.sock");

        let server = IpcServer::bind(&socket_path).unwrap();
        assert!(socket_path.exists());
        assert_eq!(server.path(), socket_path);

        // Client can connect
        let _client = UnixStream::connect(&socket_path).await.unwrap();
    }

    #[tokio::test]
    async fn test_read_write_event() {
        let tmp = tempfile::TempDir::new().unwrap();
        let socket_path = tmp.path().join("vibewatch.sock");

        let server = IpcServer::bind(&socket_path).unwrap();

        // Spawn a client that writes an event
        let path = socket_path.clone();
        let client_handle = tokio::spawn(async move {
            let mut stream = UnixStream::connect(&path).await.unwrap();
            let event = InboundEvent::Stop {
                session_id: "s42".into(),
                pid: None,
            };
            write_json(&mut stream, &event).await.unwrap();
        });

        // Server accepts and reads the event
        let stream = server.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let event = read_event(&mut reader).await.unwrap();

        match event {
            InboundEvent::Stop { session_id, pid: _ } => {
                assert_eq!(session_id, "s42");
            }
            _ => panic!("expected Stop event"),
        }

        client_handle.await.unwrap();
    }

    #[test]
    fn test_parse_permission_request_with_new_fields() {
        let json = r#"{"event":"permission_request","session_id":"s1","request_id":"r42","tool":"Bash","detail":"ls -la","pid":123}"#;
        let event: InboundEvent = serde_json::from_str(json).unwrap();
        match event {
            InboundEvent::PermissionRequest {
                session_id,
                request_id,
                tool,
                detail,
                pid,
                ..
            } => {
                assert_eq!(session_id, "s1");
                assert_eq!(request_id.as_deref(), Some("r42"));
                assert_eq!(tool.as_deref(), Some("Bash"));
                assert_eq!(detail.as_deref(), Some("ls -la"));
                assert_eq!(pid, Some(123));
            }
            _ => panic!("expected PermissionRequest"),
        }
    }

    #[test]
    fn test_parse_permission_request_without_optional_fields_still_works() {
        let json = r#"{"event":"permission_request","session_id":"s1","tool":"Bash"}"#;
        let event: InboundEvent = serde_json::from_str(json).unwrap();
        match event {
            InboundEvent::PermissionRequest {
                session_id,
                request_id,
                detail,
                pid,
                ..
            } => {
                assert_eq!(session_id, "s1");
                assert!(request_id.is_none());
                assert!(detail.is_none());
                assert!(pid.is_none());
            }
            _ => panic!("expected PermissionRequest"),
        }
    }

    #[test]
    fn test_parse_permission_request_with_suggestions() {
        let json = r#"{"event":"permission_request","session_id":"s1","request_id":"r1","tool":"Read","detail":"/etc/hosts","permission_suggestions":[{"type":"addRules","rules":[{"toolName":"Read","ruleContent":"//etc/**"}],"behavior":"allow","destination":"session"}]}"#;
        let e: InboundEvent = serde_json::from_str(json).unwrap();
        match e {
            InboundEvent::PermissionRequest { permission_suggestions, .. } => {
                assert_eq!(permission_suggestions.len(), 1);
                assert_eq!(permission_suggestions[0].kind, "addRules");
                assert_eq!(permission_suggestions[0].destination, "session");
                assert_eq!(permission_suggestions[0].rules[0].tool_name, "Read");
                assert_eq!(permission_suggestions[0].rules[0].rule_content, "//etc/**");
            }
            _ => panic!("expected PermissionRequest"),
        }
    }

    #[test]
    fn test_parse_permission_request_without_suggestions_defaults_empty() {
        let json = r#"{"event":"permission_request","session_id":"s1","tool":"Bash"}"#;
        let e: InboundEvent = serde_json::from_str(json).unwrap();
        match e {
            InboundEvent::PermissionRequest { permission_suggestions, .. } => {
                assert!(permission_suggestions.is_empty());
            }
            _ => panic!("expected PermissionRequest"),
        }
    }

    #[test]
    fn test_parse_approval_decision_with_choice_index() {
        let json = r#"{"event":"approval_decision","request_id":"r1","choice_index":2}"#;
        let e: InboundEvent = serde_json::from_str(json).unwrap();
        match e {
            InboundEvent::ApprovalDecision { request_id, choice_index } => {
                assert_eq!(request_id, "r1");
                assert_eq!(choice_index, 2);
            }
            _ => panic!("expected ApprovalDecision"),
        }
    }
}
