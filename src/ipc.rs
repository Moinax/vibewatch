use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
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
    },
    PreToolUse {
        session_id: String,
        tool: String,
        #[serde(default)]
        detail: Option<String>,
    },
    PostToolUse {
        session_id: String,
        tool: String,
        #[serde(default)]
        success: bool,
    },
    UserPromptSubmit {
        session_id: String,
        #[serde(default)]
        prompt: Option<String>,
    },
    PermissionRequest {
        session_id: String,
        #[serde(default)]
        tool: Option<String>,
    },
    PermissionDenied {
        session_id: String,
    },
    Stop {
        session_id: String,
    },
    GetStatus,
    TogglePanel,
    Subscribe,
}

/// Status response for Waybar/panel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub text: String,
    pub tooltip: String,
    pub class: String,
    pub sessions: Vec<crate::session::Session>,
}

/// Session update for streaming to panel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionUpdate {
    pub sessions: Vec<crate::session::Session>,
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
pub async fn read_event(
    stream: &mut BufReader<UnixStream>,
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
    use crate::session::{AgentKind, Session};

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
            } => {
                assert_eq!(agent, "claude_code");
                assert_eq!(session_id, "s1");
                assert_eq!(pid, 1234);
                assert!(cwd.is_none());
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
            InboundEvent::Stop { session_id } => {
                assert_eq!(session_id, "s1");
            }
            _ => panic!("expected Stop"),
        }
    }

    #[test]
    fn test_serialize_status_response() {
        let session = Session::new("s1".into(), AgentKind::ClaudeCode, 1234);
        let response = StatusResponse {
            text: "1 active".into(),
            tooltip: "Claude Code (s1): Running".into(),
            class: "active".into(),
            sessions: vec![session],
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"text\":\"1 active\""));
        assert!(json.contains("\"class\":\"active\""));
        assert!(json.contains("\"sessions\":["));
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
            };
            write_json(&mut stream, &event).await.unwrap();
        });

        // Server accepts and reads the event
        let stream = server.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let event = read_event(&mut reader).await.unwrap();

        match event {
            InboundEvent::Stop { session_id } => {
                assert_eq!(session_id, "s42");
            }
            _ => panic!("expected Stop event"),
        }

        client_handle.await.unwrap();
    }
}
