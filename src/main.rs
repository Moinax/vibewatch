mod compositor;
mod config;
mod ipc;
mod notify;
mod scanner;
mod session;
mod sound;
mod waybar;

#[cfg(feature = "panel")]
mod panel;

use std::sync::Arc;

use clap::{Parser, Subcommand};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedReadHalf;

use config::Config;
use ipc::{InboundEvent, IpcServer, SessionUpdate};
use session::{AgentKind, Session, SessionRegistry, SessionStatus};
use sound::{SoundEvent, SoundPlayer};

#[derive(Parser)]
#[command(name = "vibewatch", about = "AI agent monitor for Wayland compositors")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the vibewatch daemon
    Daemon,
    /// Send a notification event from a hook
    Notify {
        /// The event payload (JSON string)
        event: String,
        /// Agent type
        #[arg(long, default_value = "claude-code")]
        agent: String,
    },
    /// Print current session status
    Status,
    /// Toggle the overlay panel visibility
    TogglePanel,
    /// Launch the standalone GTK4 panel
    #[cfg(feature = "panel")]
    Panel,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Daemon => run_daemon().await,
        Commands::Notify { event, agent } => notify::handle_notify(&event, &agent).await,
        Commands::Status => run_status().await,
        Commands::TogglePanel => run_toggle_panel().await,
        #[cfg(feature = "panel")]
        Commands::Panel => panel::run_panel().await,
    }
}

async fn run_daemon() -> anyhow::Result<()> {
    let config = Config::load()?;
    let socket_path = config.socket_path();
    let registry = SessionRegistry::new();
    let sound_player = Arc::new(SoundPlayer::new(config.sounds.clone()));

    eprintln!(
        "vibewatch: starting daemon, socket at {}",
        socket_path.display()
    );

    let server = IpcServer::bind(&socket_path)?;

    // Spawn background scanner
    let compositor = compositor::create_compositor(&config.general.compositor)?;
    let scanner_registry = registry.clone();
    tokio::spawn(async move {
        scanner::run_scanner(scanner_registry, compositor, config).await;
    });

    eprintln!("vibewatch: daemon ready");

    // Accept loop
    loop {
        match server.accept().await {
            Ok(stream) => {
                let registry = registry.clone();
                let sound_player = sound_player.clone();
                tokio::spawn(async move {
                    handle_connection(stream, registry, sound_player).await;
                });
            }
            Err(e) => eprintln!("vibewatch: accept error: {}", e),
        }
    }
}

/// Read one JSON line from an OwnedReadHalf and parse it as an InboundEvent.
async fn read_event_from_reader(
    reader: &mut BufReader<OwnedReadHalf>,
) -> anyhow::Result<InboundEvent> {
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        anyhow::bail!("connection closed");
    }
    let event: InboundEvent = serde_json::from_str(line.trim())?;
    Ok(event)
}

/// Handle a single client connection.
async fn handle_connection(
    stream: tokio::net::UnixStream,
    registry: SessionRegistry,
    sound_player: Arc<SoundPlayer>,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    loop {
        let event = match read_event_from_reader(&mut reader).await {
            Ok(e) => e,
            Err(_) => return, // connection closed or parse error
        };

        match event {
            InboundEvent::SessionStart {
                agent,
                session_id,
                pid,
                cwd,
                session_name,
            } => {
                // Remove any scanner-created session for this PID to avoid duplicates
                registry.remove_by_pid(pid);
                let kind = parse_agent_kind(&agent);
                let mut session = Session::new(session_id, kind, pid);
                session.cwd = cwd;
                session.session_name = session_name;
                registry.register(session);
            }
            InboundEvent::PreToolUse {
                session_id,
                tool,
                detail,
            } => {
                let mut session = get_or_create_session(&registry, &session_id);
                session.status = SessionStatus::Executing;
                session.current_tool = Some(tool);
                session.tool_detail = detail;
                session.touch();
                registry.register(session);
            }
            InboundEvent::PostToolUse {
                session_id,
                tool: _,
                success,
            } => {
                let mut session = get_or_create_session(&registry, &session_id);
                session.last_tool = session.current_tool.take();
                session.last_tool_detail = session.tool_detail.take();
                session.status = SessionStatus::Thinking;
                session.touch();
                registry.register(session);
                if !success {
                    sound_player.play(SoundEvent::Error);
                }
            }
            InboundEvent::UserPromptSubmit { session_id, prompt } => {
                let mut session = get_or_create_session(&registry, &session_id);
                session.status = SessionStatus::Thinking;
                session.last_prompt = prompt;
                session.current_tool = None;
                session.tool_detail = None;
                // Refresh session name from transcript (handles /rename)
                if let Some(name) = read_transcript_name(&session_id) {
                    session.session_name = Some(name);
                }
                session.touch();
                registry.register(session);
            }
            InboundEvent::PermissionRequest { session_id, tool } => {
                let mut session = get_or_create_session(&registry, &session_id);
                session.status = SessionStatus::WaitingApproval;
                session.current_tool = tool;
                session.touch();
                registry.register(session);
                sound_player.play(SoundEvent::ApprovalNeeded);
            }
            InboundEvent::PermissionDenied { session_id } => {
                if let Some(mut session) = registry.get(&session_id) {
                    session.status = SessionStatus::Thinking;
                    session.current_tool = None;
                    session.tool_detail = None;
                    session.touch();
                    registry.register(session);
                }
            }
            InboundEvent::Stop { session_id } => {
                if let Some(mut session) = registry.get(&session_id) {
                    session.status = SessionStatus::Idle;
                    session.current_tool = None;
                    session.tool_detail = None;
                    session.touch();
                    registry.register(session);
                }
            }
            InboundEvent::GetStatus => {
                let sessions = registry.all();
                let status = waybar::build_status(&sessions);
                let mut json = serde_json::to_string(&status).unwrap_or_default();
                json.push('\n');
                let _ = write_half.write_all(json.as_bytes()).await;
                let _ = write_half.flush().await;
                return;
            }
            InboundEvent::Subscribe => {
                loop {
                    let sessions = registry.all();
                    let update = SessionUpdate { sessions };
                    let mut json = serde_json::to_string(&update).unwrap_or_default();
                    json.push('\n');
                    if write_half.write_all(json.as_bytes()).await.is_err() {
                        return; // client disconnected
                    }
                    if write_half.flush().await.is_err() {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
            InboundEvent::TogglePanel => {
                eprintln!("vibewatch: toggle-panel requested");
                // TODO: pkill -USR1 vibewatch or signal the panel process
            }
        }
    }
}

/// Get an existing session or create a new one from the session_id.
/// If a scanner session exists for this PID (found via process scan), promotes it
/// to a hook session with the correct UUID.
fn get_or_create_session(registry: &SessionRegistry, session_id: &str) -> Session {
    // Try existing session first
    if let Some(session) = registry.get(session_id) {
        return session;
    }
    // Create a new session — we don't have the PID from the event, but
    // we can find it by checking if any scanner session exists that we should promote
    let session_name = read_transcript_name(session_id);
    let mut session = Session::new(session_id.to_string(), AgentKind::ClaudeCode, 0);
    session.session_name = session_name;
    session
}

/// Find the transcript for a hook session and read its name.
fn read_transcript_name(session_id: &str) -> Option<String> {
    let claude_projects = dirs::home_dir()?.join(".claude/projects");
    // Search all project dirs for this session's transcript
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
            return None; // found transcript but no title
        }
    }
    None
}

fn parse_agent_kind(s: &str) -> AgentKind {
    match s {
        "claude_code" | "claude-code" => AgentKind::ClaudeCode,
        "codex" => AgentKind::Codex,
        "cursor" => AgentKind::Cursor,
        "webstorm" => AgentKind::WebStorm,
        _ => AgentKind::ClaudeCode,
    }
}

/// Connect to the daemon and print current status as Waybar JSON.
async fn run_status() -> anyhow::Result<()> {
    let config = Config::load()?;
    let socket_path = config.socket_path();

    match ipc::send_event(&socket_path, &InboundEvent::GetStatus).await {
        Ok(Some(response)) => {
            println!("{}", response);
        }
        Ok(None) => {
            // Daemon returned no data, print idle status
            waybar::print_waybar_status(&[]);
        }
        Err(_) => {
            // Daemon not running, print idle status
            waybar::print_waybar_status(&[]);
        }
    }

    Ok(())
}

/// Toggle the panel by checking if it's running and killing or spawning it.
async fn run_toggle_panel() -> anyhow::Result<()> {
    // Check if panel is already running
    let check = tokio::process::Command::new("pgrep")
        .args(["-f", "vibewatch panel"])
        .output()
        .await?;

    if check.status.success() {
        // Panel is running, kill it
        tokio::process::Command::new("pkill")
            .args(["-f", "vibewatch panel"])
            .output()
            .await?;
    } else {
        // Panel is not running, launch it
        let exe = std::env::current_exe()?;
        tokio::process::Command::new(exe)
            .arg("panel")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
    }
    Ok(())
}
