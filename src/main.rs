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
        Commands::Panel => {
            // Panel is not yet implemented
            eprintln!("vibewatch: panel not yet implemented");
            Ok(())
        }
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
            } => {
                let kind = parse_agent_kind(&agent);
                let session = Session::new(session_id, kind, pid);
                registry.register(session);
            }
            InboundEvent::PreToolUse {
                session_id,
                tool,
                detail,
            } => {
                if let Some(mut session) = registry.get(&session_id) {
                    session.status = SessionStatus::Executing;
                    session.current_tool = Some(tool);
                    session.tool_detail = detail;
                    session.touch();
                    registry.register(session);
                }
            }
            InboundEvent::PostToolUse {
                session_id,
                tool: _,
                success,
            } => {
                if let Some(mut session) = registry.get(&session_id) {
                    session.status = SessionStatus::Idle;
                    session.current_tool = None;
                    session.tool_detail = None;
                    session.touch();
                    registry.register(session);
                }
                if !success {
                    sound_player.play(SoundEvent::Error);
                }
            }
            InboundEvent::Stop { session_id } => {
                registry.update_status(&session_id, SessionStatus::Stopped);
                sound_player.play(SoundEvent::TaskComplete);
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

/// Send a toggle-panel event to the daemon.
async fn run_toggle_panel() -> anyhow::Result<()> {
    let config = Config::load()?;
    let socket_path = config.socket_path();

    ipc::send_event(&socket_path, &InboundEvent::TogglePanel).await?;

    Ok(())
}
