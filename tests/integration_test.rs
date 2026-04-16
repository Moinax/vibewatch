use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[tokio::test]
async fn test_daemon_ipc_flow() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sock_path = tmp.path().join("test.sock");

    // Bind server
    let server = vibewatch::ipc::IpcServer::bind(&sock_path).unwrap();
    let registry = vibewatch::session::SessionRegistry::new();

    // Spawn handler task
    let reg = registry.clone();
    let accept_task = tokio::spawn(async move {
        let stream = server.accept().await.unwrap();
        let (reader, _writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let event: vibewatch::ipc::InboundEvent = serde_json::from_str(line.trim()).unwrap();
        match event {
            vibewatch::ipc::InboundEvent::SessionStart {
                agent,
                session_id,
                pid,
                cwd: _,
                session_name: _,
            } => {
                let kind = if agent == "claude_code" {
                    vibewatch::session::AgentKind::ClaudeCode
                } else {
                    vibewatch::session::AgentKind::Codex
                };
                reg.register(vibewatch::session::Session::new(session_id, kind, pid));
            }
            _ => panic!("unexpected event"),
        }
    });

    // Send session_start
    let mut client = UnixStream::connect(&sock_path).await.unwrap();
    let event = serde_json::json!({
        "event": "session_start",
        "agent": "claude_code",
        "session_id": "test-session-1",
        "pid": 9999
    });
    client
        .write_all(serde_json::to_string(&event).unwrap().as_bytes())
        .await
        .unwrap();
    client.write_all(b"\n").await.unwrap();
    client.flush().await.unwrap();

    accept_task.await.unwrap();

    // Verify
    let sessions = registry.all();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, "test-session-1");
    assert_eq!(sessions[0].agent, vibewatch::session::AgentKind::ClaudeCode);

    let status = vibewatch::waybar::build_status(&sessions);
    assert!(status.text.contains("Claude"));
    assert!(status.tooltip.contains("Claude Code"));
}
