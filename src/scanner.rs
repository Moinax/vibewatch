use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use crate::compositor::Compositor;
use crate::config::Config;
use crate::session::{
    detect_terminal, inspect_pid_cmdline, normalize_comm, AgentKind, Session, SessionRegistry,
    CLAUDE_CODE_COMMS, CODEX_COMMS,
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

        let comm = match fs::read_to_string(format!("/proc/{}/comm", pid)) {
            Ok(c) => normalize_comm(&c),
            Err(_) => continue,
        };

        if CLAUDE_CODE_COMMS.iter().any(|n| comm == *n) {
            results.push((AgentKind::ClaudeCode, pid));
        } else if CODEX_COMMS.iter().any(|n| comm == *n) {
            results.push((AgentKind::Codex, pid));
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
        let census = claude_cwd_census(&found_processes);

        for (kind, pid) in &found_processes {
            if known_pids.contains(pid) {
                continue;
            }
            let info = inspect_pid_cmdline(*pid);
            if info.programmatic {
                continue;
            }
            let id = format!("scan-{}-{}", agent_str(kind), pid);
            let mut session = Session::new(id, *kind, *pid);
            session.session_name = info.session_name;
            session.terminal = Some(detect_terminal(*pid));
            if *kind == AgentKind::ClaudeCode {
                hydrate_from_transcript(&mut session, &census);
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
                    let known_ids: HashSet<String> = all_sessions.iter().map(|s| s.id.clone()).collect();
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

        // --- Refresh session names for hook-registered sessions (handles /rename) ---
        for session in registry.all() {
            // Only refresh hook sessions (UUID ids), not scanner sessions
            if !session.id.starts_with("scan-") && !session.id.starts_with("window-") {
                if let Some(title) = crate::session::read_transcript_name(&session.id) {
                    // Not an unconditional overwrite: a name pushed in from
                    // outside holds until this title *moves*, or this tick —
                    // which runs every couple of seconds — would undo every
                    // hand rename before the user let go of the keyboard.
                    registry.apply_agent_title(&session.id, &title);
                }
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
            if session.transcript_path.is_none() {
                let cwd = session
                    .cwd
                    .clone()
                    .map(std::path::PathBuf::from)
                    .or_else(|| std::fs::read_link(format!("/proc/{}/cwd", session.pid)).ok());
                if let (Some(home), Some(cwd)) = (dirs::home_dir(), cwd) {
                    session.transcript_path = crate::codex_rollout::find_latest_for_cwd(
                        &home.join(".codex/sessions"),
                        &cwd,
                        process_started_at(session.pid)
                            .unwrap_or(std::time::UNIX_EPOCH)
                            .checked_sub(std::time::Duration::from_secs(5))
                            .unwrap_or(std::time::UNIX_EPOCH),
                    );
                }
            }
            let Some(path) = session.transcript_path.clone() else {
                continue;
            };
            let Some(snapshot) = crate::codex_rollout::parse_file(&path) else {
                continue;
            };
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

/// How many live Claude Code processes share each cwd. Two agents in one
/// directory write to the same project directory, and nothing in a transcript
/// says which process wrote it.
fn claude_cwd_census(found: &[(AgentKind, u32)]) -> HashMap<PathBuf, usize> {
    let mut census: HashMap<PathBuf, usize> = HashMap::new();
    for (kind, pid) in found {
        if *kind != AgentKind::ClaudeCode {
            continue;
        }
        if let Ok(cwd) = fs::read_link(format!("/proc/{pid}/cwd")) {
            *census.entry(cwd).or_insert(0) += 1;
        }
    }
    census
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
fn hydrate_from_transcript(session: &mut Session, census: &HashMap<PathBuf, usize>) {
    let Ok(cwd) = fs::read_link(format!("/proc/{}/cwd", session.pid)) else {
        return;
    };
    if census.get(&cwd).copied().unwrap_or(0) > 1 {
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
    let not_before = process_started_at(session.pid)
        .unwrap_or(std::time::UNIX_EPOCH)
        .checked_sub(std::time::Duration::from_secs(5))
        .unwrap_or(std::time::UNIX_EPOCH);
    let Some(path) = crate::transcript::find_claude_transcript_for_cwd(
        &home.join(".claude/projects"),
        &cwd,
        not_before,
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
    fn census_counts_claude_pids_per_cwd_only() {
        let me = std::process::id();
        let cwd = std::fs::read_link(format!("/proc/{me}/cwd")).expect("own cwd resolves");
        let census = claude_cwd_census(&[
            (AgentKind::ClaudeCode, me),
            (AgentKind::ClaudeCode, me),
            (AgentKind::Codex, me),
            (AgentKind::ClaudeCode, u32::MAX), // no such process
        ]);
        assert_eq!(census.get(&cwd).copied(), Some(2));
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
