mod compositor;
mod config;
mod ipc;
mod install;
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
    /// Print current session status. With `--watch`, keep the socket open
    /// and stream a JSON line on every state change (waybar "continuous" mode).
    Status {
        #[arg(long)]
        watch: bool,
    },
    /// Toggle the overlay panel visibility
    TogglePanel,
    /// Install vibewatch's systemd user service and Claude Code hooks.
    Install {
        /// Skip systemd user unit install/enable.
        #[arg(long)]
        no_service: bool,
        /// Skip Claude Code hooks merge.
        #[arg(long)]
        no_hooks: bool,
        /// Print every action but change nothing on disk.
        #[arg(long)]
        dry_run: bool,
        /// Reverse the install: stop service, strip hooks, remove snippet.
        #[arg(long)]
        uninstall: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Daemon => run_daemon(),
        Commands::Notify { event, agent } => {
            cli_runtime()?.block_on(notify::handle_notify(&event, &agent))
        }
        Commands::Status { watch } => cli_runtime()?.block_on(run_status(watch)),
        Commands::TogglePanel => cli_runtime()?.block_on(run_toggle_panel()),
        Commands::Install { no_service, no_hooks, dry_run, uninstall } => {
            install::run(install::Options {
                no_service,
                no_hooks,
                dry_run,
                uninstall,
            })?;
            Ok(())
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
    daemon_runtime()?.block_on(run_daemon_headless(config, registry))
}

/// Cap tokio workers: the default = one per CPU, which is wasteful for a
/// daemon whose workload is sporadic IPC plus a couple of tickers.
fn daemon_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
}

/// Short-lived CLI commands (`notify`, `status`, `toggle-panel`) do one
/// socket I/O and exit; a multi-thread runtime would spawn one idle worker
/// per CPU for nothing.
fn cli_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
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

    let status_notify: Arc<tokio::sync::Notify> = Arc::new(tokio::sync::Notify::new());

    let compositor = compositor::create_compositor(&config.general.compositor)?;
    let scanner_registry = registry.clone();
    let scanner_notify = status_notify.clone();
    tokio::spawn(async move {
        scanner::run_scanner(scanner_registry, compositor, config, scanner_notify).await;
    });

    eprintln!("vibewatch: daemon ready (headless)");

    let approval_registry = crate::approval::ApprovalRegistry::new();

    let reaper_registry = registry.clone();
    let reaper_approval = approval_registry.clone();
    let reaper_notify = status_notify.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
        ticker.tick().await; // skip first immediate tick
        loop {
            ticker.tick().await;
            let stale = reaper_approval
                .reap_stale(std::time::Duration::from_secs(580))
                .await;
            for entry in stale {
                eprintln!(
                    "vibewatch: reaping stale approval for session {}",
                    entry.session_id
                );
                if let Some(mut s) = reaper_registry.get(&entry.session_id) {
                    s.pending_approval = None;
                    s.status = SessionStatus::Thinking;
                    reaper_registry.register(s);
                    reaper_notify.notify_waiters();
                }
                // Dropping `entry` closes the write half so the hook read returns EOF.
            }
        }
    });

    loop {
        match server.accept().await {
            Ok(stream) => {
                let registry = registry.clone();
                let sound_player = sound_player.clone();
                let approval_registry = approval_registry.clone();
                let status_notify = status_notify.clone();
                tokio::spawn(async move {
                    handle_connection(
                        stream,
                        registry,
                        sound_player,
                        None::<Arc<dyn Fn() + Send + Sync>>,
                        None::<Arc<dyn Fn() + Send + Sync>>,
                        approval_registry,
                        status_notify,
                    )
                    .await;
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

        let show_weak = glib::SendWeakRef::from(window.downgrade());
        let show_fn: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let show_weak = show_weak.clone();
            glib::MainContext::default().invoke(move || {
                if let Some(win) = show_weak.upgrade() {
                    win.set_visible(true);
                    win.present();
                }
            });
        });

        let config = config.clone();
        let registry = registry.clone();
        std::thread::spawn(move || {
            let rt = daemon_runtime().expect("failed to create tokio runtime");
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

                let status_notify: Arc<tokio::sync::Notify> = Arc::new(tokio::sync::Notify::new());

                let scanner_registry = registry.clone();
                let scanner_notify = status_notify.clone();
                tokio::spawn(async move {
                    scanner::run_scanner(scanner_registry, compositor, config, scanner_notify).await;
                });

                eprintln!("vibewatch: daemon ready");

                let approval_registry = crate::approval::ApprovalRegistry::new();

                let reaper_registry = registry.clone();
                let reaper_approval = approval_registry.clone();
                let reaper_notify = status_notify.clone();
                tokio::spawn(async move {
                    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
                    ticker.tick().await; // skip first immediate tick
                    loop {
                        ticker.tick().await;
                        let stale = reaper_approval
                            .reap_stale(std::time::Duration::from_secs(580))
                            .await;
                        for entry in stale {
                            eprintln!(
                                "vibewatch: reaping stale approval for session {}",
                                entry.session_id
                            );
                            if let Some(mut s) = reaper_registry.get(&entry.session_id) {
                                s.pending_approval = None;
                                s.status = SessionStatus::Thinking;
                                reaper_registry.register(s);
                                reaper_notify.notify_waiters();
                            }
                            // Dropping `entry` closes the write half so the hook read returns EOF.
                        }
                    }
                });

                loop {
                    match server.accept().await {
                        Ok(stream) => {
                            let registry = registry.clone();
                            let sound_player = sound_player.clone();
                            let toggle_fn = toggle_fn.clone();
                            let show_fn = show_fn.clone();
                            let approval_registry = approval_registry.clone();
                            let status_notify = status_notify.clone();
                            tokio::spawn(async move {
                                handle_connection(
                                    stream,
                                    registry,
                                    sound_player,
                                    Some(toggle_fn),
                                    Some(show_fn),
                                    approval_registry,
                                    status_notify,
                                )
                                .await;
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
    show_sender: Option<Arc<dyn Fn() + Send + Sync>>,
    approval_registry: crate::approval::ApprovalRegistry,
    status_notify: Arc<tokio::sync::Notify>,
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
                if session::inspect_pid_cmdline(pid).programmatic {
                    continue;
                }
                let kind = parse_agent_kind(&agent);
                let mut session = Session::new(session_id, kind, pid);
                session.cwd = cwd;
                session.session_name = session_name;
                session.terminal = Some(session::detect_terminal(pid));
                registry.register(session);
                status_notify.notify_waiters();
            }
            InboundEvent::PreToolUse {
                session_id,
                tool,
                detail,
                pid,
            } => {
                if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
                    let prev = session.status;
                    // AskUserQuestion blocks on the user from the moment it
                    // runs — not every shape fires a follow-up permission-request
                    // hook (multi-question / multiSelect don't, empirically),
                    // so flip straight to WaitingApproval here so the waybar
                    // flips to attention even when no permission-request arrives.
                    session.status = if tool == crate::session::TOOL_ASK_USER_QUESTION {
                        SessionStatus::WaitingApproval
                    } else {
                        SessionStatus::Executing
                    };
                    session.current_tool = Some(tool.clone());
                    session.tool_detail = detail;
                    session.touch();
                    log_transition(&session.id, prev, session.status, &format!("tool={}", tool));
                    registry.register(session);
                    status_notify.notify_waiters();
                } else {
                    log_drop("PreToolUse", &session_id, pid);
                }
            }
            InboundEvent::PostToolUse {
                session_id,
                tool: _,
                success,
                pid,
            } => {
                if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
                    let prev = session.status;
                    if session.pending_approval.is_some() {
                        release_held_approvals(&approval_registry, &session.id).await;
                        session.pending_approval = None;
                    }
                    session.last_tool = session.current_tool.take();
                    session.last_tool_detail = session.tool_detail.take();
                    session.last_tool_at = now_epoch();
                    session.status = SessionStatus::Thinking;
                    let agent = session.agent;
                    if let Some(text) = transcript::read_last_assistant_line(
                        agent,
                        &session_id,
                        &mut session.transcript_path,
                    ) {
                        session.set_last_agent_text_if_changed(text);
                    }
                    session.touch();
                    log_transition(&session.id, prev, session.status, "PostToolUse");
                    registry.register(session);
                    status_notify.notify_waiters();
                } else {
                    log_drop("PostToolUse", &session_id, pid);
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
                    let prev = session.status;
                    if session.pending_approval.is_some() {
                        release_held_approvals(&approval_registry, &session.id).await;
                        session.pending_approval = None;
                    }
                    session.status = SessionStatus::Thinking;
                    session.last_prompt = prompt;
                    session.last_prompt_at = now_epoch();
                    session.current_tool = None;
                    session.tool_detail = None;
                    if let Some(name) = session::read_transcript_name(&session_id) {
                        session.session_name = Some(name);
                    }
                    session.touch();
                    log_transition(&session.id, prev, session.status, "UserPromptSubmit");
                    registry.register(session);
                    status_notify.notify_waiters();
                } else {
                    log_drop("UserPromptSubmit", &session_id, pid);
                }
            }
            InboundEvent::PermissionRequest {
                session_id,
                request_id,
                tool,
                detail,
                pid,
                permission_suggestions,
                option_labels,
            } => {
                eprintln!(
                    "vibewatch: recv PermissionRequest session={} request_id={:?} tool={:?} pid={:?} suggestions={} option_labels={:?}",
                    session_id, request_id, tool, pid,
                    serde_json::to_string(&permission_suggestions).unwrap_or_default(),
                    option_labels,
                );
                let request_id = match request_id {
                    Some(r) => r,
                    None => {
                        // Old fire-and-forget caller: just flip status and continue.
                        if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
                            session.status = SessionStatus::WaitingApproval;
                            session.current_tool = tool;
                            session.touch();
                            registry.register(session);
                            status_notify.notify_waiters();
                            sound_player.play(SoundEvent::ApprovalNeeded);
                        }
                        continue;
                    }
                };
                let tool_name = tool.clone().unwrap_or_else(|| "tool".into());

                let choices = if option_labels.is_empty() {
                    crate::session::ApprovalChoice::build_from(
                        &tool_name,
                        &permission_suggestions,
                    )
                } else {
                    crate::session::ApprovalChoice::from_labels(&option_labels)
                };
                let no_choices = choices.is_empty();

                if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
                    let prev = session.status;
                    // Any prior prompt is moot now — release its held socket
                    // before we overwrite `pending_approval`.
                    if session.pending_approval.is_some() {
                        release_held_approvals(&approval_registry, &session.id).await;
                    }
                    session.status = SessionStatus::WaitingApproval;
                    session.current_tool = Some(tool_name.clone());
                    session.tool_detail = detail.clone();
                    session.pending_approval = Some(crate::session::PendingApproval {
                        request_id: request_id.clone(),
                        tool: tool_name,
                        detail,
                        choices,
                    });
                    session.touch();
                    log_transition(&session.id, prev, session.status, "PermissionRequest");
                    registry.register(session);
                    status_notify.notify_waiters();
                } else {
                    log_drop("PermissionRequest", &session_id, pid);
                }
                sound_player.play(SoundEvent::ApprovalNeeded);
                if let Some(ref show) = show_sender {
                    show();
                }

                // No choices ⇒ the hook already short-circuited with `ask`
                // and closed the socket; nothing to answer back.
                if no_choices {
                    drop(write_half);
                    return;
                }

                // Move write_half into the registry and exit the handler.
                let entry = crate::approval::ApprovalEntry {
                    write_half,
                    session_id,
                    created_at: std::time::Instant::now(),
                };
                approval_registry.insert(request_id, entry).await;
                return;
            }
            InboundEvent::PermissionDenied { session_id, pid } => {
                if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
                    session.status = SessionStatus::Thinking;
                    session.current_tool = None;
                    session.tool_detail = None;
                    session.pending_approval = None;
                    session.touch();
                    registry.register(session);
                    status_notify.notify_waiters();
                }
            }
            InboundEvent::ApprovalDecision { request_id, choice_index } => {
                eprintln!(
                    "vibewatch: recv ApprovalDecision request_id={} choice_index={}",
                    request_id, choice_index
                );
                let Some(entry) = approval_registry.take(&request_id).await else {
                    eprintln!(
                        "vibewatch: NO entry in ApprovalRegistry for request_id={}",
                        request_id
                    );
                    continue;
                };
                let chosen = registry
                    .get(&entry.session_id)
                    .and_then(|s| s.pending_approval.as_ref().and_then(|p| p.choices.get(choice_index).cloned()));
                let (label, behavior_str, suggestion, updated_permissions) = match chosen {
                    Some(c) => (c.label, c.behavior, c.suggestion, c.updated_permissions),
                    None => {
                        eprintln!(
                            "vibewatch: no choice at index {} for request_id={}; denying",
                            choice_index, request_id
                        );
                        ("".to_string(), "deny".to_string(), None, None)
                    }
                };
                let response_json = serde_json::json!({
                    "label": label,
                    "behavior": behavior_str,
                    "suggestion": suggestion,
                    "updatedPermissions": updated_permissions,
                });
                let mut line = response_json.to_string();
                line.push('\n');
                let mut wh = entry.write_half;
                match wh.write_all(line.as_bytes()).await {
                    Ok(_) => eprintln!(
                        "vibewatch: wrote decision line for request_id={}: {}",
                        request_id,
                        line.trim()
                    ),
                    Err(e) => eprintln!(
                        "vibewatch: failed to write approval decision for {}: {}",
                        request_id, e
                    ),
                }
                if let Err(e) = wh.flush().await {
                    eprintln!(
                        "vibewatch: failed to flush approval decision for {}: {}",
                        request_id, e
                    );
                }
                if let Some(mut s) = registry.get(&entry.session_id) {
                    s.pending_approval = None;
                    s.status = SessionStatus::Thinking;
                    s.current_tool = None;
                    s.tool_detail = None;
                    s.touch();
                    registry.register(s);
                    status_notify.notify_waiters();
                }
            }
            InboundEvent::Stop { session_id, pid } => {
                if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
                    let prev = session.status;
                    if session.pending_approval.is_some() {
                        release_held_approvals(&approval_registry, &session.id).await;
                    }
                    session.status = SessionStatus::Idle;
                    session.current_tool = None;
                    session.tool_detail = None;
                    session.pending_approval = None;
                    let agent = session.agent;
                    if let Some(text) = transcript::read_last_assistant_line(
                        agent,
                        &session_id,
                        &mut session.transcript_path,
                    ) {
                        session.set_last_agent_text_if_changed(text);
                    }
                    session.touch();
                    log_transition(&session.id, prev, session.status, "Stop");
                    registry.register(session);
                    status_notify.notify_waiters();
                } else {
                    log_drop("Stop", &session_id, pid);
                }
                let registry = registry.clone();
                let sid = session_id.clone();
                let late_notify = status_notify.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                    if let Some(mut session) = registry.get(&sid) {
                        let agent = session.agent;
                        if let Some(text) = transcript::read_last_assistant_line(
                            agent,
                            &sid,
                            &mut session.transcript_path,
                        ) {
                            if session.set_last_agent_text_if_changed(text) {
                                registry.register(session);
                                late_notify.notify_waiters();
                            }
                        }
                    }
                });
            }
            InboundEvent::GetStatus => {
                let sessions = registry.all();
                let status = waybar::build_status(&sessions);
                let mut json = waybar_payload(&status);
                json.push('\n');
                let _ = write_half.write_all(json.as_bytes()).await;
                let _ = write_half.flush().await;
                return;
            }
            InboundEvent::SubscribeStatus => {
                // Push one line per state change. The initial line is emitted
                // immediately so the subscriber doesn't wait for the next
                // transition to render something.
                let mut last_payload = String::new();
                loop {
                    let sessions = registry.all();
                    let status = waybar::build_status(&sessions);
                    let mut json = waybar_payload(&status);
                    if json != last_payload {
                        last_payload = json.clone();
                        json.push('\n');
                        if write_half.write_all(json.as_bytes()).await.is_err() {
                            return;
                        }
                        if write_half.flush().await.is_err() {
                            return;
                        }
                    }
                    status_notify.notified().await;
                }
            }
            InboundEvent::TogglePanel => {
                if let Some(ref sender) = toggle_sender {
                    sender();
                }
            }
        }
    }
}

/// Look up a session for a hook event. If `pid` is provided and the id is not
/// found, try to adopt a same-PID session (handles daemon restart while an
/// agent is already running, and sibling SessionStart events on the same PID).
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

/// Release any approval socket held for `session_id`. Call this when a
/// subsequent session event (PostToolUse, UserPromptSubmit, Stop, a new
/// PermissionRequest) proves the prior prompt was already answered —
/// typically because the user responded in the Claude Code TUI instead of the
/// widget.
///
/// Dropping the write_halves closes the held sockets; the hook's blocked
/// `read_line` returns EOF and falls back to emitting `{behavior:"ask"}`,
/// which Claude ignores since it already moved past the prompt.
///
/// The caller is expected to also null-out `session.pending_approval` on its
/// local `Session` copy before calling `registry.register(session)`.
async fn release_held_approvals(
    approval_registry: &crate::approval::ApprovalRegistry,
    session_id: &str,
) {
    let entries = approval_registry.take_by_session(session_id).await;
    if !entries.is_empty() {
        eprintln!(
            "vibewatch: releasing {} held approval socket(s) for session={} — resolved externally",
            entries.len(),
            session_id
        );
        drop(entries);
    }
}

fn log_transition(session_id: &str, prev: SessionStatus, next: SessionStatus, ctx: &str) {
    eprintln!(
        "vibewatch: trans session={} {:?} -> {:?} ({})",
        session_id, prev, next, ctx
    );
}

fn log_drop(event: &str, session_id: &str, pid: Option<u32>) {
    eprintln!(
        "vibewatch: DROP {} session={} pid={:?} — no session found",
        event, session_id, pid
    );
}

/// Serialize a `StatusResponse` as the JSON payload waybar consumes. `class`
/// is a single-element array so waybar replaces the widget's class list each
/// update instead of accumulating stale classes.
fn waybar_payload(status: &ipc::StatusResponse) -> String {
    serde_json::json!({
        "text": status.text,
        "class": [status.class],
    })
    .to_string()
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
async fn run_status(watch: bool) -> anyhow::Result<()> {
    let config = Config::load()?;
    let socket_path = config.socket_path();

    if watch {
        return run_status_watch(&socket_path).await;
    }

    // Bounded wait: if the daemon hangs, waybar would hang too and accumulate
    // stalled `status` subprocesses, undoing the fire-and-forget fix.
    let timeout = std::time::Duration::from_secs(2);
    let request = ipc::request_response(&socket_path, &InboundEvent::GetStatus);
    match tokio::time::timeout(timeout, request).await {
        Ok(Ok(Some(response))) => println!("{}", response),
        _ => waybar::print_waybar_status(&[]),
    }

    Ok(())
}

/// Streaming subscriber: keep forwarding daemon-pushed JSON lines to stdout
/// forever. Reconnects on socket drops (daemon restart) so waybar's
/// continuous custom-module stays alive across daemon upgrades.
async fn run_status_watch(socket_path: &std::path::Path) -> anyhow::Result<()> {
    const RETRY: std::time::Duration = std::time::Duration::from_secs(2);

    loop {
        // Either a clean close (Ok) or a connect/read failure (Err) means the
        // widget should show offline until we reconnect.
        let _ = stream_once(socket_path).await;
        emit_offline();
        tokio::time::sleep(RETRY).await;
    }
}

async fn stream_once(socket_path: &std::path::Path) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path).await?;

    let event = serde_json::to_string(&InboundEvent::SubscribeStatus)?;
    stream.write_all(event.as_bytes()).await?;
    stream.write_all(b"\n").await?;
    stream.flush().await?;

    let (read_half, _write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half).lines();
    while let Some(line) = reader.next_line().await? {
        println!("{}", line);
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }
    Ok(())
}

fn emit_offline() {
    waybar::print_waybar_status(&[]);
    use std::io::Write;
    let _ = std::io::stdout().flush();
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
