mod approval;
mod codex_rollout;
mod compositor;
mod config;
mod install;
mod ipc;
mod mute;
mod notify;
mod scanner;
mod session;
mod sound;
mod t3;
mod transcript;
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
#[command(
    name = "vibewatch",
    about = "AI agent monitor for Wayland compositors",
    version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("VIBEWATCH_GIT_SHA"), ")")
)]
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
        /// Which slice of the line to emit. `all` is the single-module layout;
        /// the others exist so a `group/` of child widgets can give each piece
        /// its own CSS — one stream per child, since waybar cannot feed two
        /// widgets from one `exec`.
        #[arg(long, value_enum, default_value_t = ipc::StatusPart::All)]
        part: ipc::StatusPart,
    },
    /// Toggle the overlay panel visibility
    TogglePanel,
    /// Name a session yourself, overriding the agent's own title. Meant for a
    /// multiplexer hook: when the user renames an agent's tab by hand, this is
    /// how the panel and the bar hear about it. The name holds until the agent's
    /// own title changes, and then the agent gets the say back.
    Rename {
        /// The agent session id (Claude Code's `session_id`).
        session_id: String,
        /// The name to show.
        name: String,
    },
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
        Commands::Status { watch, part } => cli_runtime()?.block_on(run_status(watch, part)),
        Commands::TogglePanel => cli_runtime()?.block_on(run_toggle_panel()),
        Commands::Rename { session_id, name } => {
            cli_runtime()?.block_on(run_rename(session_id, name))
        }
        Commands::Install {
            no_service,
            no_hooks,
            dry_run,
            uninstall,
        } => {
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
        eprintln!(
            "vibewatch: WAYLAND_DISPLAY set but panel feature not compiled; running headless"
        );
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
    let timing = AnnounceTiming::from_config(&config);
    let policy = ConnectionPolicy {
        timing,
        t3: config.t3,
    };

    eprintln!(
        "vibewatch: starting daemon (headless), socket at {}",
        socket_path.display()
    );

    let server = IpcServer::bind(&socket_path)?;

    let status_notify: Arc<tokio::sync::Notify> = Arc::new(tokio::sync::Notify::new());

    let compositor = compositor::create_compositor(&config.general.compositor)?;
    let scanner_registry = registry.clone();
    let scanner_notify = status_notify.clone();
    let scanner_finish_registry = registry.clone();
    let scanner_sound = sound_player.clone();
    let scanner_hooks = PanelHooks::default();
    let scanner_finished = Arc::new(move |sid, seq| {
        schedule_announce(
            &scanner_finish_registry,
            &scanner_sound,
            &scanner_hooks,
            sid,
            seq,
            timing.idle_debounce,
        );
    });
    tokio::spawn(async move {
        scanner::run_scanner(
            scanner_registry,
            compositor,
            config,
            scanner_notify,
            scanner_finished,
        )
        .await;
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
                        PanelHooks::default(),
                        approval_registry,
                        status_notify,
                        policy,
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
    use adw::prelude::*;
    use gtk4::glib;
    use libadwaita as adw;

    panel::prefer_software_renderer();

    let app = adw::Application::builder()
        .application_id("app.vibewatch.daemon")
        .build();

    // A second `vibewatch daemon` does not start up on its own: GApplication
    // hands its launch to the instance already holding the application id,
    // which fires `activate` again. Build everything exactly once — a second
    // pass would stack another panel on the layer surface, take the IPC socket
    // away from the first one (`IpcServer::bind` unlinks a live socket) and run
    // a second scanner inside the same process.
    let started = std::cell::Cell::new(false);
    app.connect_activate(move |app| {
        if started.replace(true) {
            eprintln!("vibewatch: daemon already running, ignoring activation");
            return;
        }

        let window = panel::create_panel(app, registry.clone(), config.panel.clone());

        // SendWeakRef is Send+Sync; actual widget access happens only inside
        // glib::MainContext::invoke(), which runs on the GTK main thread.
        let win_weak = glib::SendWeakRef::from(window.downgrade());

        let toggle_fn: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let win_weak = win_weak.clone();
            glib::MainContext::default().invoke(move || {
                if let Some(win) = win_weak.upgrade() {
                    panel::toggle(&win);
                }
            });
        });

        let show_weak = glib::SendWeakRef::from(window.downgrade());
        let show_fn: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let show_weak = show_weak.clone();
            glib::MainContext::default().invoke(move || {
                if let Some(win) = show_weak.upgrade() {
                    panel::show(&win);
                }
            });
        });

        let panel_hooks = PanelHooks {
            toggle: Some(toggle_fn),
            show: Some(show_fn.clone()),
            finish: config.panel.open_on_finish.then_some(show_fn),
        };

        let config = config.clone();
        let registry = registry.clone();
        std::thread::spawn(move || {
            let rt = daemon_runtime().expect("failed to create tokio runtime");
            rt.block_on(async move {
                let socket_path = config.socket_path();
                let sound_player = Arc::new(SoundPlayer::new(config.sounds.clone()));
                let timing = AnnounceTiming::from_config(&config);
                let policy = ConnectionPolicy {
                    timing,
                    t3: config.t3,
                };

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
                let scanner_finish_registry = registry.clone();
                let scanner_sound = sound_player.clone();
                let scanner_hooks = panel_hooks.clone();
                let scanner_finished = Arc::new(move |sid, seq| {
                    schedule_announce(
                        &scanner_finish_registry,
                        &scanner_sound,
                        &scanner_hooks,
                        sid,
                        seq,
                        timing.idle_debounce,
                    );
                });
                tokio::spawn(async move {
                    scanner::run_scanner(
                        scanner_registry,
                        compositor,
                        config,
                        scanner_notify,
                        scanner_finished,
                    )
                    .await;
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
                            let panel_hooks = panel_hooks.clone();
                            let approval_registry = approval_registry.clone();
                            let status_notify = status_notify.clone();
                            tokio::spawn(async move {
                                handle_connection(
                                    stream,
                                    registry,
                                    sound_player,
                                    panel_hooks,
                                    approval_registry,
                                    status_notify,
                                    policy,
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

/// What a connection can ask the overlay panel to do. Every field is `Some`
/// in GTK mode and `None` in headless mode. The callbacks are type-erased to
/// `Arc<dyn Fn() + Send + Sync>` so this compiles without GTK feature flags.
#[derive(Clone, Default)]
struct PanelHooks {
    /// Flip the drawer open/closed — the waybar click.
    toggle: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Slide the drawer open because an agent is blocked on the user.
    show: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Same thing, on an agent finishing its turn. Held separately from
    /// `show` so `panel.open_on_finish = false` drops it without disarming
    /// the approval pop-up.
    finish: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// The two waits that stand between a `Stop` and its announcement. Both are
/// read once at daemon start and handed to every connection.
#[derive(Clone, Copy)]
struct AnnounceTiming {
    /// Quiet required of a turn with nothing outstanding.
    idle_debounce: std::time::Duration,
    /// Patience for the sub-agents a held turn is waiting on.
    hold_ceiling: std::time::Duration,
}

impl AnnounceTiming {
    fn from_config(config: &Config) -> Self {
        Self {
            idle_debounce: config.idle_debounce(),
            hold_ceiling: config.hold_ceiling(),
        }
    }
}

/// The slice of the config a client connection has to answer from: when a
/// finished turn is announced, and whether the agents T3 Code runs are sessions
/// at all. Carried as one value because the hook handler is the only reader of
/// either and takes enough arguments already.
#[derive(Clone, Copy)]
struct ConnectionPolicy {
    timing: AnnounceTiming,
    t3: crate::config::T3Config,
}

/// Announce that an agent finished its turn: the chime, and the panel popping
/// open with the just-finished row ranked to the top. The sound says *someone*
/// is done, the panel says which one.
fn announce_finish(sound_player: &SoundPlayer, panel_hooks: &PanelHooks, why: &str) {
    eprintln!("vibewatch: announcing finish ({why})");
    sound_player.play(SoundEvent::Idle);
    if let Some(ref show) = panel_hooks.finish {
        show();
    }
}

/// What a `Stop` turned out to mean, once the session behind it was found.
enum StopOutcome {
    /// A turn with nothing outstanding: announce it if it stays quiet.
    Ready(String, u64),
    /// Sub-agents are still running. The last one to report in re-opens the
    /// question; announcing now would be the false positive being fixed. If none
    /// of them ever reports, the hold ceiling releases it.
    Held(String, u64),
    /// No session matched the stop, so there is nothing to watch and no evidence
    /// anything resumed. Announce it — silence is the worse failure.
    Unknown,
}

/// Announce that `sid` finished, once it has been quiet for `idle_debounce`.
///
/// The wait is what separates a turn that ended from a turn that ended and was
/// picked straight back up — including the moment after the last sub-agent
/// reports in, when the main agent is about to be re-invoked and the count alone
/// would say "done". Suppression requires positive evidence that the session
/// moved on; every other outcome announces, because swallowing a real
/// completion is worse than one chime too many.
fn schedule_announce(
    registry: &SessionRegistry,
    sound_player: &Arc<SoundPlayer>,
    panel_hooks: &PanelHooks,
    sid: String,
    seq: u64,
    idle_debounce: std::time::Duration,
) {
    if idle_debounce.is_zero() {
        announce_finish(sound_player, panel_hooks, "undebounced");
        return;
    }
    let registry = registry.clone();
    let sound_player = sound_player.clone();
    let panel_hooks = panel_hooks.clone();
    tokio::spawn(async move {
        tokio::time::sleep(idle_debounce).await;
        match registry.get(&sid) {
            // Still announceable, and still the turn this was scheduled for.
            Some(s) if s.announceable() && s.finish_seq == seq => {
                announce_finish(&sound_player, &panel_hooks, "settled")
            }
            Some(s) => eprintln!(
                "vibewatch: idle announcement for {sid} superseded (status {}, turn {} vs {seq}, {} sub-agents)",
                s.status.css_class(),
                s.finish_seq,
                s.pending_agents
            ),
            // Gone from the registry: the agent finished and its process exited
            // inside the window — a one-shot `claude -p`, or a pane that closed
            // — and the scanner pruned it. That is a finish, not a resumption.
            None => announce_finish(&sound_player, &panel_hooks, "exited"),
        }
    });
}

/// Announce a turn that outstanding sub-agents are holding open, if `hold_ceiling`
/// passes without any of them reporting in.
///
/// The hold itself is unbounded by design — a fan-out legitimately runs for
/// minutes — but a sub-agent that dies on a terminal API error, or in a pane that
/// was killed, never sends the `Stop` that would release it. Without this the
/// turn stays held and *silent* until the next user prompt resets the count, and
/// a chime that never comes is a worse failure than one too many.
///
/// A live sub-agent's own tool hooks land on the parent session, so anything
/// still running keeps that session out of `Idle` and cancels this — which is
/// what lets the wait be minutes long without bringing the false chimes back.
/// Only a session still sitting on the very turn that was held, still waiting on
/// sub-agents, and still untouched since, is released. Releasing it also drops
/// the count: those sub-agents are now presumed gone, so the next `Stop` is a
/// finish rather than another hold.
fn schedule_hold_ceiling(
    registry: &SessionRegistry,
    sound_player: &Arc<SoundPlayer>,
    panel_hooks: &PanelHooks,
    sid: String,
    seq: u64,
    hold_ceiling: std::time::Duration,
) {
    if hold_ceiling.is_zero() {
        return;
    }
    let registry = registry.clone();
    let sound_player = sound_player.clone();
    let panel_hooks = panel_hooks.clone();
    tokio::spawn(async move {
        tokio::time::sleep(hold_ceiling).await;
        // Gone from the registry: the process exited while held, which the user
        // did themselves (a closed pane) — nothing to announce minutes later.
        let Some(mut session) = registry.get(&sid) else {
            return;
        };
        if !session.just_finished() || session.finish_seq != seq {
            eprintln!(
                "vibewatch: hold on {sid} ended on its own (status {}, turn {} vs {seq})",
                session.status.css_class(),
                session.finish_seq
            );
            return;
        }
        let dropped = session.reset_subagents();
        if dropped == 0 {
            eprintln!("vibewatch: hold on {sid} already released by its last sub-agent");
            return;
        }
        registry.register(session);
        announce_finish(
            &sound_player,
            &panel_hooks,
            &format!("hold expired, {dropped} sub-agent(s) never reported"),
        );
    });
}

/// Handle a single client connection.
async fn handle_connection(
    stream: tokio::net::UnixStream,
    registry: SessionRegistry,
    sound_player: Arc<SoundPlayer>,
    panel_hooks: PanelHooks,
    approval_registry: crate::approval::ApprovalRegistry,
    status_notify: Arc<tokio::sync::Notify>,
    policy: ConnectionPolicy,
) {
    let ConnectionPolicy { timing, t3 } = policy;
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
                let hosts = if t3.enabled {
                    crate::t3::live_runtimes()
                } else {
                    Vec::new()
                };
                if !session::is_trackable_agent(pid, &hosts) {
                    continue;
                }
                let kind = parse_agent_kind(&agent);
                let mut session = Session::new(session_id.clone(), kind, pid);
                session.cwd = cwd;
                session.session_name = session_name;
                session.terminal = Some(session::detect_terminal(pid));
                registry.register(session);
                // A background job starts as a fork of the session that launched
                // it, and takes over a turn already in flight. The pane it came
                // from never ends that turn, so quiet it here — this is the one
                // moment anything knows the two are the same piece of work.
                if let Some(parent) =
                    session::fork_parent_session_id(pid).filter(|parent| *parent != session_id)
                {
                    if registry.hand_off(&parent) {
                        eprintln!("vibewatch: {parent} handed its turn to {session_id}");
                    }
                }
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
                    // The tool returns as soon as the sub-agent is spawned, so its
                    // PostToolUse says nothing about whether that work is done —
                    // only the sub-agent's own Stop does.
                    if session::spawns_subagent(&tool) {
                        session.launch_subagent();
                        eprintln!(
                            "vibewatch: {} launched a sub-agent ({} outstanding)",
                            session.id, session.pending_agents
                        );
                    }
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
                success: _,
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
                    session.last_tool_at = Some(session::now_epoch());
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
                // No sound on tool completion/errors — they fire constantly
                // during normal work. Alerts are limited to approval requests
                // and going idle (see PermissionRequest and Stop handlers).
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
                    session.last_prompt_at = Some(session::now_epoch());
                    session.current_tool = None;
                    session.tool_detail = None;
                    // A new prompt is a clean slate: an agent still running from
                    // the previous turn cannot re-invoke this one now anyway,
                    // since the user is driving it again.
                    let dropped = session.reset_subagents();
                    if dropped > 0 {
                        eprintln!(
                            "vibewatch: {} prompted with {dropped} sub-agent(s) still counted — resetting",
                            session.id
                        );
                    }
                    // Same rule as the scan tick: the agent's title only takes
                    // the name if it has moved since a hand rename banked it.
                    if let Some(title) = session::read_transcript_name(&session_id) {
                        session.offer_agent_title(&title);
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
                    crate::session::ApprovalChoice::build_from(&tool_name, &permission_suggestions)
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
                if let Some(ref show) = panel_hooks.show {
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
            InboundEvent::ApprovalDecision {
                request_id,
                choice_index,
            } => {
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
                let chosen = registry.get(&entry.session_id).and_then(|s| {
                    s.pending_approval
                        .as_ref()
                        .and_then(|p| p.choices.get(choice_index).cloned())
                });
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
                // Set by the lookup below to the session that finished and the
                // turn it finished on, so the delayed announcement can check
                // both are still true when it fires.
                let mut finished = StopOutcome::Unknown;
                if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
                    let prev = session.status;
                    if session.pending_approval.is_some() {
                        release_held_approvals(&approval_registry, &session.id).await;
                    }
                    session.status = SessionStatus::Idle;
                    session.current_tool = None;
                    session.tool_detail = None;
                    session.pending_approval = None;
                    session.mark_finished();
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
                    // With sub-agents still running this stop is the agent parking
                    // itself until they report back, and the last to report is
                    // what re-opens the question.
                    finished = if session.pending_agents == 0 {
                        StopOutcome::Ready(session.id.clone(), session.finish_seq)
                    } else {
                        eprintln!(
                            "vibewatch: {} stopped with {} sub-agent(s) outstanding — holding",
                            session.id, session.pending_agents
                        );
                        StopOutcome::Held(session.id.clone(), session.finish_seq)
                    };
                    registry.register(session);
                    status_notify.notify_waiters();
                } else {
                    log_drop("Stop", &session_id, pid);
                }
                // The agent finished responding and went idle. Pop the panel
                // open alongside the chime: the sound says *someone* is done,
                // the panel says which one — the just-finished row is ranked
                // to the top and highlighted for a few seconds.
                //
                // Two things gate it, covering gaps neither could alone:
                // outstanding sub-agents hold it for however many minutes they
                // run, which no timeout can do; and idle_debounce_ms then covers
                // the second or two in which a stop gets picked straight back up.
                // A hold is itself bounded by hold_ceiling_ms, so sub-agents that
                // die without reporting can't swallow the finish entirely.
                //
                // The status flip above is deliberately not delayed — the widget
                // stays honest about the current state.
                match finished {
                    StopOutcome::Ready(sid, seq) => schedule_announce(
                        &registry,
                        &sound_player,
                        &panel_hooks,
                        sid,
                        seq,
                        timing.idle_debounce,
                    ),
                    StopOutcome::Held(sid, seq) => schedule_hold_ceiling(
                        &registry,
                        &sound_player,
                        &panel_hooks,
                        sid,
                        seq,
                        timing.hold_ceiling,
                    ),
                    StopOutcome::Unknown => {
                        announce_finish(&sound_player, &panel_hooks, "no session")
                    }
                }
            }
            InboundEvent::SubAgentStop { session_id, pid } => {
                // Deliberately does not touch status: a sub-agent finishing says
                // nothing about what the main agent is doing, and the whole point
                // of counting is to stop these from flapping the session to Idle.
                if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
                    // The last one to report re-opens the question the held stop
                    // left hanging. Still debounced from there, because the agent
                    // is about to be re-invoked if it has more to do.
                    let was_last = session.subagent_finished();
                    let settling = was_last && session.announceable();
                    let left = session.pending_agents;
                    let sid = session.id.clone();
                    let seq = session.finish_seq;
                    registry.register(session);
                    eprintln!("vibewatch: sub-agent of {sid} reported ({left} outstanding)");
                    if settling {
                        schedule_announce(
                            &registry,
                            &sound_player,
                            &panel_hooks,
                            sid,
                            seq,
                            timing.idle_debounce,
                        );
                    }
                } else {
                    log_drop("SubAgentStop", &session_id, pid);
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
            InboundEvent::GetStatus { part } => {
                let sessions = registry.all();
                let status = waybar::build_status(&sessions);
                let mut json = waybar::payload_part(&status, part);
                json.push('\n');
                let _ = write_half.write_all(json.as_bytes()).await;
                let _ = write_half.flush().await;
                return;
            }
            InboundEvent::SubscribeStatus { part } => {
                // Push one line per state change. The initial line is emitted
                // immediately so the subscriber doesn't wait for the next
                // transition to render something.
                //
                // De-duplicating on the *part* and not the whole line matters
                // once a bar runs several children: a name change must not wake
                // the state widget, or every child would redraw on every event.
                let mut last_payload = String::new();
                loop {
                    let sessions = registry.all();
                    let status = waybar::build_status(&sessions);
                    let mut json = waybar::payload_part(&status, part);
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
            InboundEvent::AcknowledgeSession { session_id } => {
                if let Some(mut session) = registry.get(&session_id) {
                    if session.just_finished() {
                        session.acknowledge();
                        registry.register(session);
                        status_notify.notify_waiters();
                    }
                }
            }
            InboundEvent::TogglePanel => {
                if let Some(ref sender) = panel_hooks.toggle {
                    sender();
                }
            }
            InboundEvent::SetSessionName { session_id, name } => {
                // The agent's title as it stands goes in with the name, so the
                // hold ends on the next change to it rather than on everything
                // it has already said.
                let title = session::read_transcript_name(&session_id);
                if registry.set_name_from_outside(&session_id, name.clone(), title) {
                    eprintln!("vibewatch: {session_id} named \"{name}\" from outside");
                    status_notify.notify_waiters();
                } else {
                    log_drop("SetSessionName", &session_id, None);
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
async fn run_status(watch: bool, part: ipc::StatusPart) -> anyhow::Result<()> {
    let config = Config::load()?;
    let socket_path = config.socket_path();

    if watch {
        return run_status_watch(&socket_path, part).await;
    }

    // Bounded wait: if the daemon hangs, waybar would hang too and accumulate
    // stalled `status` subprocesses, undoing the fire-and-forget fix.
    let timeout = std::time::Duration::from_secs(2);
    let event = InboundEvent::GetStatus { part };
    let request = ipc::request_response(&socket_path, &event);
    match tokio::time::timeout(timeout, request).await {
        Ok(Ok(Some(response))) => println!("{}", response),
        _ => waybar::print_waybar_part(&[], part),
    }

    Ok(())
}

/// Streaming subscriber: keep forwarding daemon-pushed JSON lines to stdout
/// forever. Reconnects on socket drops (daemon restart) so waybar's
/// continuous custom-module stays alive across daemon upgrades.
async fn run_status_watch(
    socket_path: &std::path::Path,
    part: ipc::StatusPart,
) -> anyhow::Result<()> {
    const RETRY: std::time::Duration = std::time::Duration::from_secs(2);

    loop {
        // Either a clean close (Ok) or a connect/read failure (Err) means the
        // widget should show offline until we reconnect.
        let _ = stream_once(socket_path, part).await;
        emit_offline(part);
        tokio::time::sleep(RETRY).await;
    }
}

async fn stream_once(socket_path: &std::path::Path, part: ipc::StatusPart) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path).await?;

    let event = serde_json::to_string(&InboundEvent::SubscribeStatus { part })?;
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

fn emit_offline(part: ipc::StatusPart) {
    waybar::print_waybar_part(&[], part);
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

/// Push a name in for one session. Silent on success, and never fatal: this
/// runs from a hook on somebody's turn end, so a daemon that is not up must
/// cost a warning and nothing else.
async fn run_rename(session_id: String, name: String) -> anyhow::Result<()> {
    let config = Config::load()?;
    let event = InboundEvent::SetSessionName { session_id, name };
    if let Err(e) = ipc::send_event(&config.socket_path(), &event).await {
        eprintln!("vibewatch: failed to rename session: {}", e);
        eprintln!("vibewatch: is the daemon running?");
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
