mod compositor;
mod config;
mod ipc;
mod notify;
mod approval;
mod scanner;
mod session;
mod transcript;
mod sound;
mod waybar;

#[cfg(feature = "panel")]
mod panel;

use std::sync::Arc;

use clap::{Parser, Subcommand};
use tokio::io::{AsyncWriteExt, BufReader};

use config::Config;
use ipc::{InboundEvent, IpcServer};
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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Daemon => run_daemon(),
        Commands::Notify { event, agent } => {
            tokio::runtime::Runtime::new()?.block_on(notify::handle_notify(&event, &agent))
        }
        Commands::Status => {
            tokio::runtime::Runtime::new()?.block_on(run_status())
        }
        Commands::TogglePanel => {
            tokio::runtime::Runtime::new()?.block_on(run_toggle_panel())
        }
    }
}

fn run_daemon() -> anyhow::Result<()> {
    let config = Config::load()?;
    let registry = SessionRegistry::new();

    // Check if we have a graphical session for the panel
    let has_display = std::env::var("WAYLAND_DISPLAY").is_ok();

    if has_display {
        #[cfg(feature = "panel")]
        return run_daemon_with_panel(config, registry);

        #[cfg(not(feature = "panel"))]
        eprintln!("vibewatch: WAYLAND_DISPLAY set but panel feature not compiled; running headless");
    } else {
        eprintln!("vibewatch: no WAYLAND_DISPLAY, running in headless mode (no panel)");
    }
    tokio::runtime::Runtime::new()?.block_on(run_daemon_headless(config, registry))
}

/// Headless daemon: pure tokio loop, no GTK. Used when WAYLAND_DISPLAY is unset.
async fn run_daemon_headless(config: Config, registry: SessionRegistry) -> anyhow::Result<()> {
    let socket_path = config.socket_path();
    let sound_player = Arc::new(SoundPlayer::new(config.sounds.clone()));

    eprintln!(
        "vibewatch: starting daemon (headless), socket at {}",
        socket_path.display()
    );

    let server = IpcServer::bind(&socket_path)?;

    let compositor = compositor::create_compositor(&config.general.compositor)?;
    let scanner_registry = registry.clone();
    tokio::spawn(async move {
        scanner::run_scanner(scanner_registry, compositor, config).await;
    });

    eprintln!("vibewatch: daemon ready (headless)");

    loop {
        match server.accept().await {
            Ok(stream) => {
                let registry = registry.clone();
                let sound_player = sound_player.clone();
                tokio::spawn(async move {
                    handle_connection(stream, registry, sound_player, None::<Arc<dyn Fn() + Send + Sync>>).await;
                });
            }
            Err(e) => eprintln!("vibewatch: accept error: {}", e),
        }
    }
}

/// GTK-driven daemon: adw::Application is the outer loop, tokio runs on a background thread.
#[cfg(feature = "panel")]
fn run_daemon_with_panel(config: Config, registry: SessionRegistry) -> anyhow::Result<()> {
    use libadwaita as adw;
    use adw::prelude::*;
    use gtk4::glib;

    let app = adw::Application::builder()
        .application_id("app.vibewatch.daemon")
        .build();

    app.connect_activate(move |app| {
        let window = panel::create_panel(app, registry.clone());

        // SendWeakRef is Send+Sync; actual widget access happens only inside
        // glib::MainContext::invoke(), which runs on the GTK main thread.
        let win_weak = glib::SendWeakRef::from(window.downgrade());

        let toggle_fn: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let win_weak = win_weak.clone();
            glib::MainContext::default().invoke(move || {
                if let Some(win) = win_weak.upgrade() {
                    win.set_visible(!win.is_visible());
                }
            });
        });

        let config = config.clone();
        let registry = registry.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
            rt.block_on(async move {
                let socket_path = config.socket_path();
                let sound_player = Arc::new(SoundPlayer::new(config.sounds.clone()));

                eprintln!(
                    "vibewatch: starting daemon, socket at {}",
                    socket_path.display()
                );

                let server = match IpcServer::bind(&socket_path) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("vibewatch: failed to bind socket: {}", e);
                        return;
                    }
                };

                let compositor = match compositor::create_compositor(&config.general.compositor) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("vibewatch: failed to create compositor: {}", e);
                        return;
                    }
                };

                let scanner_registry = registry.clone();
                tokio::spawn(async move {
                    scanner::run_scanner(scanner_registry, compositor, config).await;
                });

                eprintln!("vibewatch: daemon ready");

                loop {
                    match server.accept().await {
                        Ok(stream) => {
                            let registry = registry.clone();
                            let sound_player = sound_player.clone();
                            let toggle_fn = toggle_fn.clone();
                            tokio::spawn(async move {
                                handle_connection(stream, registry, sound_player, Some(toggle_fn)).await;
                            });
                        }
                        Err(e) => eprintln!("vibewatch: accept error: {}", e),
                    }
                }
            });
        });
    });

    app.run_with_args::<String>(&[]);
    Ok(())
}

/// Handle a single client connection.
///
/// `toggle_sender` is `Some` when running with a panel (GTK mode), `None` in headless mode.
/// The sender type is erased to `Arc<dyn Fn() + Send + Sync>` so this function compiles
/// without GTK feature flags.
async fn handle_connection(
    stream: tokio::net::UnixStream,
    registry: SessionRegistry,
    sound_player: Arc<SoundPlayer>,
    toggle_sender: Option<Arc<dyn Fn() + Send + Sync>>,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    loop {
        let event = match ipc::read_event(&mut reader).await {
            Ok(e) => e,
            Err(_) => return,
        };

        match event {
            InboundEvent::SessionStart {
                agent,
                session_id,
                pid,
                cwd,
                session_name,
            } => {
                registry.remove_by_pid(pid);
                let kind = parse_agent_kind(&agent);
                let mut session = Session::new(session_id, kind, pid);
                session.cwd = cwd;
                session.session_name = session_name;
                session.terminal = Some(session::detect_terminal(pid));
                registry.register(session);
            }
            InboundEvent::PreToolUse {
                session_id,
                tool,
                detail,
                pid,
            } => {
                if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
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
                pid,
            } => {
                if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
                    session.last_tool = session.current_tool.take();
                    session.last_tool_detail = session.tool_detail.take();
                    session.status = SessionStatus::Thinking;
                    let agent = session.agent;
                    if let Some(text) = transcript::read_last_assistant_line(
                        agent,
                        &session_id,
                        &mut session.transcript_path,
                    ) {
                        session.last_agent_text = Some(text);
                        session.last_agent_text_at = now_epoch();
                    }
                    session.touch();
                    registry.register(session);
                }
                if !success {
                    sound_player.play(SoundEvent::Error);
                }
            }
            InboundEvent::UserPromptSubmit {
                session_id,
                prompt,
                pid,
            } => {
                if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
                    session.status = SessionStatus::Thinking;
                    session.last_prompt = prompt;
                    session.last_prompt_at = now_epoch();
                    session.current_tool = None;
                    session.tool_detail = None;
                    if let Some(name) = session::read_transcript_name(&session_id) {
                        session.session_name = Some(name);
                    }
                    session.touch();
                    registry.register(session);
                }
            }
            InboundEvent::PermissionRequest {
                session_id,
                request_id: _,
                tool,
                detail: _,
                pid,
            } => {
                if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
                    session.status = SessionStatus::WaitingApproval;
                    session.current_tool = tool;
                    session.touch();
                    registry.register(session);
                    sound_player.play(SoundEvent::ApprovalNeeded);
                }
            }
            InboundEvent::PermissionDenied { session_id, pid } => {
                if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
                    session.status = SessionStatus::Thinking;
                    session.current_tool = None;
                    session.tool_detail = None;
                    session.touch();
                    registry.register(session);
                }
            }
            InboundEvent::Stop { session_id, pid } => {
                if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
                    session.status = SessionStatus::Idle;
                    session.current_tool = None;
                    session.tool_detail = None;
                    let agent = session.agent;
                    if let Some(text) = transcript::read_last_assistant_line(
                        agent,
                        &session_id,
                        &mut session.transcript_path,
                    ) {
                        session.last_agent_text = Some(text);
                        session.last_agent_text_at = now_epoch();
                    }
                    session.touch();
                    registry.register(session);
                }
                // Claude Code flushes the final assistant text block to its
                // JSONL transcript slightly after the Stop hook fires. Re-read
                // once after a short delay so the panel picks up that last
                // message instead of the next-to-last one.
                let registry = registry.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                    if let Some(mut session) = registry.get(&session_id) {
                        let agent = session.agent;
                        if let Some(text) = transcript::read_last_assistant_line(
                            agent,
                            &session_id,
                            &mut session.transcript_path,
                        ) {
                            session.last_agent_text = Some(text);
                            session.last_agent_text_at = now_epoch();
                            registry.register(session);
                        }
                    }
                });
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
            InboundEvent::TogglePanel => {
                if let Some(ref sender) = toggle_sender {
                    sender();
                }
            }
            InboundEvent::ApprovalDecision { .. } => {
                // Handled in Task 7.
            }
        }
    }
}

/// Look up a session for a hook event. If `pid` is provided and the id is not
/// found, try to adopt a matching `scan-*` session (handles daemon restart while
/// an agent is already running).
fn lookup_session(
    registry: &SessionRegistry,
    session_id: &str,
    pid: Option<u32>,
) -> Option<Session> {
    if let Some(pid) = pid {
        registry.get_or_adopt(session_id, pid)
    } else {
        registry.get(session_id)
    }
}

fn now_epoch() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
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
            waybar::print_waybar_status(&[]);
        }
        Err(_) => {
            waybar::print_waybar_status(&[]);
        }
    }

    Ok(())
}

/// Toggle the panel by sending a TogglePanel IPC event to the daemon.
async fn run_toggle_panel() -> anyhow::Result<()> {
    let config = Config::load()?;
    let socket_path = config.socket_path();

    if let Err(e) = ipc::send_event(&socket_path, &InboundEvent::TogglePanel).await {
        eprintln!("vibewatch: failed to toggle panel: {}", e);
        eprintln!("vibewatch: is the daemon running?");
    }

    Ok(())
}
