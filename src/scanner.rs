use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use crate::compositor::Compositor;
use crate::config::Config;
use crate::session::{
    detect_terminal, identify_agent_pid, is_programmatic_pid, AgentKind, Session, SessionRegistry,
};

/// Map an AgentKind to its short string identifier.
fn agent_str(kind: &AgentKind) -> &'static str {
    match kind {
        AgentKind::ClaudeCode => "claude",
        AgentKind::Codex => "codex",
        AgentKind::Cursor => "cursor",
        AgentKind::WebStorm => "webstorm",
    }
}

/// Codex has no Stop hook, so its equivalent finish edge is inferred from the
/// durable rollout state. Requiring a working predecessor prevents a newly
/// discovered, already-idle process from producing a startup chime.
fn is_codex_finish_transition(
    previous: crate::session::SessionStatus,
    next: crate::session::SessionStatus,
) -> bool {
    matches!(
        previous,
        crate::session::SessionStatus::Thinking | crate::session::SessionStatus::Executing
    ) && next == crate::session::SessionStatus::Idle
}

/// Scan /proc for running CLI agent processes.
/// Returns a list of (AgentKind, pid) tuples for recognised agents.
///
/// What counts as an agent is [`identify_agent_pid`]'s call, shared with the
/// registry's liveness check so discovery and reaping cannot disagree.
pub fn scan_agent_processes() -> Vec<(AgentKind, u32)> {
    let mut results = Vec::new();

    let entries = match fs::read_dir("/proc") {
        Ok(e) => e,
        Err(_) => return results,
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Only look at numeric directory names (PIDs)
        let pid: u32 = match name_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        if let Some(kind) = identify_agent_pid(pid) {
            results.push((kind, pid));
        }
    }

    results
}

/// Background scanner loop. Runs every 3 seconds, discovering CLI agent
/// processes via /proc and GUI agent windows via the compositor.
///
/// `status_notify` is pulsed at the end of every iteration so the waybar
/// `SubscribeStatus` subscriber learns about sessions that disappeared when
/// their PID died — those removals bypass the hook handler entirely.
pub async fn run_scanner(
    registry: SessionRegistry,
    compositor: Box<dyn Compositor>,
    config: Config,
    status_notify: std::sync::Arc<tokio::sync::Notify>,
    codex_finished: std::sync::Arc<dyn Fn(String, u64) + Send + Sync>,
) {
    // Transcript mtime per session, as of the last time its title was read.
    // Reading a title means reading a whole transcript (see
    // `read_transcript_name_at` for why it cannot be tailed) and those reach
    // tens of megabytes, so on a three-second tick it has to be skipped unless
    // the file actually moved. Lives across ticks, and is pruned with the
    // sessions it keys on.
    let mut title_mtime: HashMap<String, std::time::SystemTime> = HashMap::new();
    loop {
        // Remove sessions whose PID is no longer alive
        registry.cleanup_dead();
        // Collapse ghost rows: a long-lived agent process rotates through
        // multiple session ids (/clear, resume, compaction), leaving stale
        // same-PID sessions that cleanup_dead can't reap (the PID is still
        // alive). Keep one session per live CLI PID.
        registry.dedupe_cli_pids();

        // --- CLI agent scanning ---
        let found_processes = scan_agent_processes();
        let all_sessions = registry.all();
        let known_pids: HashSet<u32> = all_sessions.iter().map(|s| s.pid).collect();
        // Built on the first Claude Code process this tick has never seen. In
        // steady state that is none of them, and a readlink per live agent every
        // three seconds is not worth paying for an answer nobody reads.
        let mut census: Option<HashMap<u32, PathBuf>> = None;

        for (kind, pid) in &found_processes {
            if known_pids.contains(pid) {
                continue;
            }
            if is_programmatic_pid(*pid) {
                continue;
            }
            let id = format!("scan-{}-{}", agent_str(kind), pid);
            let mut session = Session::new(id, *kind, *pid);
            session.terminal = Some(detect_terminal(*pid));
            if *kind == AgentKind::ClaudeCode {
                let census = census.get_or_insert_with(|| claude_cwd_census(&found_processes));
                hydrate_from_transcript(&mut session, census);
            }
            registry.register(session);
        }

        // --- Window-based agent scanning ---
        for (name, agent_config) in &config.agents {
            let kind = match name.as_str() {
                "cursor" => AgentKind::Cursor,
                "webstorm" => AgentKind::WebStorm,
                _ => continue,
            };

            match compositor.find_by_class(&agent_config.window_class).await {
                Ok(windows) => {
                    let current_window_ids: HashSet<String> =
                        windows.iter().map(|w| w.id.clone()).collect();

                    // Register new windows
                    let known_ids: HashSet<String> =
                        all_sessions.iter().map(|s| s.id.clone()).collect();
                    for win in &windows {
                        let id = format!("window-{}-{}", name, win.id);
                        if !known_ids.contains(&id) {
                            let mut session = Session::new(id, kind, win.pid);
                            session.window_id = Some(win.id.clone());
                            registry.register(session);
                        }
                    }

                    // Remove stale window sessions for this agent
                    let prefix = format!("window-{}-", name);
                    for session in registry.all() {
                        if session.id.starts_with(&prefix) {
                            let win_id = session.id.strip_prefix(&prefix).unwrap_or("");
                            if !current_window_ids.contains(win_id) {
                                registry.remove(&session.id);
                            }
                        }
                    }
                }
                Err(_) => {
                    // Compositor query failed; skip this agent this cycle
                }
            }
        }

        // --- Update window_ids for CLI agent sessions via PID matching ---
        // Use candidate PIDs (agent ancestry + Zellij/herdr client ancestry)
        // so agents running inside a Zellij or herdr session — children of the
        // shared server, not the terminal window — still resolve to their window.
        for session in registry.all() {
            if session.id.starts_with("scan-") && session.window_id.is_none() {
                let candidates = crate::session::window_candidate_pids(session.pid);
                if let Ok(Some(win)) = compositor.find_by_pids(&candidates).await {
                    registry.set_window_id(&session.id, win.id);
                }
            }
        }

        // --- Refresh session names from the agent's own title (handles /rename) ---
        // Scanner-discovered sessions count too, keyed by the transcript id
        // hydration found for them: an agent the scanner saw before any hook
        // fired — a muxer resuming one, or any agent idle since the daemon
        // started — otherwise never gets a name of its own and shows the folder
        // it runs in for as long as it stays quiet.
        let live_ids: HashSet<String> = registry.all().iter().map(|s| s.id.clone()).collect();
        title_mtime.retain(|id, _| live_ids.contains(id));
        for session in registry.all() {
            if let Some(title) = transcript_title_if_changed(&session, &mut title_mtime)
                .or_else(|| muxer_name(&session))
            {
                // Not an unconditional overwrite: a name pushed in from
                // outside holds until this title *moves*, or this tick —
                // which runs every couple of seconds — would undo every
                // hand rename before the user let go of the keyboard.
                registry.apply_agent_title(&session.id, &title);
            }
        }

        // Codex has no Claude-style lifecycle hooks. Its CLI writes a rollout
        // JSONL containing task/tool transitions, so attach the newest rollout
        // for this process's cwd and reduce it into the same Session model.
        for mut session in registry
            .all()
            .into_iter()
            .filter(|s| s.agent == AgentKind::Codex)
        {
            // The open fd is authoritative and is checked every tick because a
            // long-lived Codex process rotates rollout files without changing
            // PID. It also disambiguates several Codex processes sharing cwd.
            // Fall back to the cwd search during the short window before Codex
            // opens its writer, or on platforms without procfs fd links.
            if let Some(path) = crate::codex_rollout::find_open_for_pid(session.pid) {
                session.transcript_path = Some(path);
            } else if session.transcript_path.is_none() {
                let cwd = session
                    .cwd
                    .clone()
                    .map(std::path::PathBuf::from)
                    .or_else(|| std::fs::read_link(format!("/proc/{}/cwd", session.pid)).ok());
                if let (Some(home), Some(cwd)) = (dirs::home_dir(), cwd) {
                    session.transcript_path = crate::codex_rollout::find_latest_for_cwd(
                        &home.join(".codex/sessions"),
                        &cwd,
                        transcript_floor(session.pid),
                    );
                }
            }
            let Some(path) = session.transcript_path.clone() else {
                continue;
            };
            let Some(snapshot) = crate::codex_rollout::parse_file(&path) else {
                continue;
            };
            session.agent_session_id = Some(snapshot.session_id.clone());
            let finished_turn = is_codex_finish_transition(session.status, snapshot.status);
            session.cwd = snapshot.cwd;
            session.status = snapshot.status;
            session.current_tool = snapshot.current_tool;
            session.tool_detail = snapshot.tool_detail;
            if snapshot.last_tool.is_some() {
                session.last_tool = snapshot.last_tool;
                session.last_tool_at = std::fs::metadata(&path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
            }
            if snapshot.last_prompt.is_some() {
                session.last_prompt = snapshot.last_prompt;
                session.last_prompt_at = session.last_tool_at;
            }
            if let Some(text) = crate::transcript::read_last_assistant_line(
                AgentKind::Codex,
                &snapshot.session_id,
                &mut session.transcript_path,
            ) {
                session.set_last_agent_text_if_changed(text);
            }
            let finished = if finished_turn {
                session.mark_finished();
                session.touch();
                Some((session.id.clone(), session.finish_seq))
            } else {
                None
            };
            registry.register(session);
            if let Some((session_id, finish_seq)) = finished {
                codex_finished(session_id, finish_seq);
            }
        }

        status_notify.notify_waiters();
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}

/// Linux process start time as wall-clock time. `/proc/<pid>/stat` stores
/// clock ticks since boot (USER_HZ is 100 on Linux), while `/proc/stat`
/// exposes the boot epoch. This prevents a newly launched Codex process from
/// being paired with an old rollout from another session in the same cwd.
fn process_started_at(pid: u32) -> Option<std::time::SystemTime> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 2..];
    // Field 22 overall; `rest` begins at field 3, so starttime is index 19.
    let ticks: u64 = rest.split_whitespace().nth(19)?.parse().ok()?;
    let proc_stat = std::fs::read_to_string("/proc/stat").ok()?;
    let boot_epoch: u64 = proc_stat
        .lines()
        .find_map(|line| line.strip_prefix("btime "))?
        .parse()
        .ok()?;
    Some(
        std::time::UNIX_EPOCH
            + std::time::Duration::from_secs(boot_epoch)
            + std::time::Duration::from_millis(ticks.saturating_mul(10)),
    )
}

/// The cwd of every live Claude Code process. Resolved once for the whole tick
/// because the answer is needed twice: to find a process's project directory,
/// and to tell whether another live agent shares it — two agents in one
/// directory write to the same project directory, and nothing in a transcript
/// says which process wrote it.
fn claude_cwd_census(found: &[(AgentKind, u32)]) -> HashMap<u32, PathBuf> {
    found
        .iter()
        .filter(|(kind, _)| *kind == AgentKind::ClaudeCode)
        .filter_map(|(_, pid)| Some((*pid, fs::read_link(format!("/proc/{pid}/cwd")).ok()?)))
        .collect()
}

/// Is another live Claude Code process working in the same directory as `pid`?
/// If so nothing can say which of them wrote the newest transcript there, and
/// the honest answer is to attach neither.
fn cwd_is_shared(census: &HashMap<u32, PathBuf>, pid: u32) -> bool {
    let Some(cwd) = census.get(&pid) else {
        return false;
    };
    census
        .iter()
        .any(|(other, dir)| *other != pid && dir == cwd)
}

/// A session's own title, read only when its transcript has moved since the last
/// look. `None` for an agent with no Claude transcript, and for one whose
/// transcript is unchanged — the name it already carries is still that title, so
/// there is nothing to re-apply.
///
/// The gate is the point: reading a title means reading a whole transcript (see
/// `read_transcript_name_at`), which runs to tens of megabytes on a long
/// session, and this is a three-second tick over every session. An `mtime` stat
/// is a few microseconds, and a new title appends a record, so it is an exact
/// change signal rather than a heuristic.
///
/// Scanner-discovered sessions are included, keyed by the transcript id
/// hydration recorded for them. They used to be skipped outright, so an agent
/// the scanner saw before any hook fired — a muxer resuming one, or any agent
/// idle since the daemon started — never picked up a name of its own. They are
/// also the population that makes the gate matter, being the quiet ones.
fn transcript_title_if_changed(
    session: &Session,
    seen: &mut HashMap<String, std::time::SystemTime>,
) -> Option<String> {
    if session.agent != AgentKind::ClaudeCode {
        return None; // read_transcript_name_at only knows Claude's layout
    }
    // Hydration caches the path for a scanner session; a hook session is keyed
    // by the id Claude knows it as, so its path can be resolved from that.
    // `window-` sessions are GUI agents and have no transcript at all.
    let path = match session.transcript_path.clone() {
        Some(path) => path,
        None if session.id.starts_with("scan-") || session.id.starts_with("window-") => {
            return None
        }
        None => crate::session::claude_transcript_path(&session.id)?,
    };
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
    if seen.get(&session.id) == Some(&mtime) {
        return None;
    }
    seen.insert(session.id.clone(), mtime);
    crate::session::read_transcript_name_at(&path)
}

/// The name the muxer hosting this agent has for it — the fallback for a session
/// whose transcript could not be identified, which is what happens whenever two
/// live agents share a working directory. `herdr_pane_title` owns why the muxer
/// is the only thing that can answer that, and why it is authoritative.
///
/// Deliberately gated on the session having no name at all: it is a round trip
/// per call, and this loop runs every three seconds against every session. Once
/// a name has landed, herdr's own hook pushes any later change in (`vibewatch
/// rename`), so re-asking would buy nothing. A daemon restart drops the name and
/// this fills it straight back in, which the push cannot do for an idle agent.
fn muxer_name(session: &Session) -> Option<String> {
    if session.session_name.is_some() {
        return None;
    }
    let pane = crate::session::herdr_pane_of(session.pid)?;
    crate::session::herdr_pane_title(&pane)
}

/// The oldest transcript mtime that can belong to `pid`, with a few seconds of
/// slack for the gap between exec and the first write.
///
/// This is what keeps a long-dead session's transcript — there are dozens per
/// project — from being pinned on a process that has written nothing yet.
fn transcript_floor(pid: u32) -> std::time::SystemTime {
    process_started_at(pid)
        .unwrap_or(std::time::UNIX_EPOCH)
        .checked_sub(std::time::Duration::from_secs(5))
        .unwrap_or(std::time::UNIX_EPOCH)
}

/// Re-derive a freshly discovered Claude Code session's state from its
/// transcript.
///
/// Discovery means one of two things, and the hooks can help with neither: the
/// daemon restarted under a running agent, or it started after one. Either way
/// the session arrives at `Idle` and the next hook is the first thing that could
/// correct that — which, for an agent blocked on a question, is the answer
/// itself, however many minutes later. Hence reading the state off the durable
/// record instead, the way Codex sessions have always been read.
///
/// Deliberately not a chime: `finished_at` stays untouched, so a session found
/// sitting idle is not announced as having just finished.
///
/// Quiet on failure by design — no cwd, no project directory, a transcript
/// older than the process — because `Idle` is the honest answer when the record
/// says nothing. The one case that gets a log line is two agents sharing a
/// directory: that one we could have guessed at and chose not to, since the
/// wrong row lit up is worse than no row lit up.
fn hydrate_from_transcript(session: &mut Session, census: &HashMap<u32, PathBuf>) {
    let Some(cwd) = census.get(&session.pid) else {
        return;
    };
    if cwd_is_shared(census, session.pid) {
        eprintln!(
            "vibewatch: {} shares {} with another live agent — leaving its state to the hooks",
            session.id,
            cwd.display()
        );
        return;
    }
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let Some(path) = crate::transcript::find_claude_transcript_for_cwd(
        &home.join(".claude/projects"),
        cwd,
        transcript_floor(session.pid),
    ) else {
        return;
    };
    let Some(snapshot) = crate::transcript::snapshot_claude_file(&path) else {
        return;
    };
    session.cwd = Some(cwd.to_string_lossy().into_owned());
    session.status = snapshot.status;
    session.current_tool = snapshot.current_tool;
    session.tool_detail = snapshot.tool_detail;
    session.transcript_path = Some(path);
    // The agent's own session id, which a `scan-` session is not keyed by. The
    // Codex path records it for the same reason. Two things need it: the name
    // refresh below, so a scanner-discovered agent tracks its own title, and
    // `set_name_from_outside`, whose fallback matches on this — without it every
    // `vibewatch rename <session-id>` from a multiplexer hook was dropped as
    // "no session found" for any agent the scanner found first.
    session.agent_session_id = Some(snapshot.session_id.clone());
    // Caching the path is what makes the text readable at all: the reader
    // resolves by session id, which a `scan-` session does not have.
    if let Some(text) = crate::transcript::read_last_assistant_line(
        AgentKind::ClaudeCode,
        &snapshot.session_id,
        &mut session.transcript_path,
    ) {
        session.set_last_agent_text_if_changed(text);
    }
    eprintln!(
        "vibewatch: {} re-derived as {} from transcript of {}",
        session.id,
        session.status.css_class(),
        snapshot.session_id
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scan_agent_processes_does_not_panic() {
        let results = scan_agent_processes();
        // The result may be empty in test environments; we just verify it doesn't crash
        let _ = results;
    }

    /// The gate that keeps a multi-megabyte transcript read off a three-second
    /// tick: the first look reads, an unchanged file is skipped, and a file that
    /// moved is read again.
    #[test]
    fn transcript_title_is_read_once_per_change() {
        let dir = std::env::temp_dir().join(format!("vibewatch-title-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"custom-title\",\"customTitle\":\"first\"}\n",
        )
        .unwrap();

        let mut session = Session::new("scan-claude-77".into(), AgentKind::ClaudeCode, 77);
        session.transcript_path = Some(path.clone());
        let mut seen = HashMap::new();

        assert_eq!(
            transcript_title_if_changed(&session, &mut seen).as_deref(),
            Some("first")
        );
        assert_eq!(
            transcript_title_if_changed(&session, &mut seen),
            None,
            "an unchanged transcript must not be re-read"
        );

        // A retitle appends, which moves mtime. Retried rather than slept on:
        // the clock is nanosecond-resolution here, but a coarse filesystem could
        // land the rewrite in the same tick and make the assertion vacuous.
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        for _ in 0..50 {
            std::fs::write(
                &path,
                "{\"type\":\"custom-title\",\"customTitle\":\"second\"}\n",
            )
            .unwrap();
            if std::fs::metadata(&path).unwrap().modified().unwrap() != before {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            transcript_title_if_changed(&session, &mut seen).as_deref(),
            Some("second"),
            "a transcript that moved must be read again"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A Codex thread is not a Claude transcript and a GUI agent has none, so
    /// neither may reach the reader — which would otherwise sweep every project
    /// directory to find nothing.
    #[test]
    fn transcript_title_skips_agents_with_no_claude_transcript() {
        let mut seen = HashMap::new();
        let mut codex = Session::new("scan-codex-42".into(), AgentKind::Codex, 42);
        codex.agent_session_id = Some("thread-abc".into());
        assert_eq!(transcript_title_if_changed(&codex, &mut seen), None);

        let window = Session::new("window-cursor-3".into(), AgentKind::ClaudeCode, 3);
        assert_eq!(transcript_title_if_changed(&window, &mut seen), None);

        // A scanner session whose transcript hydration never located: nothing to
        // read, and the muxer fallback is what names it.
        let scan = Session::new("scan-claude-78".into(), AgentKind::ClaudeCode, 78);
        assert_eq!(transcript_title_if_changed(&scan, &mut seen), None);
    }

    #[test]
    fn codex_working_to_idle_is_a_finish() {
        assert!(is_codex_finish_transition(
            crate::session::SessionStatus::Thinking,
            crate::session::SessionStatus::Idle,
        ));
        assert!(is_codex_finish_transition(
            crate::session::SessionStatus::Executing,
            crate::session::SessionStatus::Idle,
        ));
    }

    #[test]
    fn the_census_resolves_live_claude_pids_and_skips_the_rest() {
        let me = std::process::id();
        let cwd = std::fs::read_link(format!("/proc/{me}/cwd")).expect("own cwd resolves");
        let census = claude_cwd_census(&[
            (AgentKind::ClaudeCode, me),
            (AgentKind::Codex, me),
            (AgentKind::ClaudeCode, u32::MAX), // no such process
        ]);
        assert_eq!(census.get(&me), Some(&cwd));
        assert_eq!(
            census.len(),
            1,
            "a pid with no /proc entry contributes none"
        );
    }

    #[test]
    fn a_directory_is_shared_only_when_another_live_agent_is_in_it() {
        let mine = PathBuf::from("/home/dev/api");
        let theirs = PathBuf::from("/home/dev/web");
        let census = HashMap::from([(1, mine.clone()), (2, theirs), (3, mine)]);
        assert!(cwd_is_shared(&census, 1), "pid 3 is in the same directory");
        assert!(!cwd_is_shared(&census, 2), "alone in its own");
        assert!(
            !cwd_is_shared(&census, 9),
            "a pid the census never resolved"
        );
    }

    #[test]
    fn codex_initial_or_repeated_idle_does_not_chime() {
        assert!(!is_codex_finish_transition(
            crate::session::SessionStatus::Running,
            crate::session::SessionStatus::Idle,
        ));
        assert!(!is_codex_finish_transition(
            crate::session::SessionStatus::Idle,
            crate::session::SessionStatus::Idle,
        ));
    }
}
