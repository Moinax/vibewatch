use std::collections::HashSet;
use std::fs;

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
) {
    loop {
        // Remove sessions whose PID is no longer alive
        registry.cleanup_dead();

        // --- CLI agent scanning ---
        let found_processes = scan_agent_processes();
        let all_sessions = registry.all();
        let known_pids: HashSet<u32> = all_sessions.iter().map(|s| s.pid).collect();

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
        for session in registry.all() {
            if session.id.starts_with("scan-") && session.window_id.is_none() {
                if let Ok(Some(win)) = compositor.find_by_pid(session.pid).await {
                    registry.set_window_id(&session.id, win.id);
                }
            }
        }

        // --- Refresh session names for hook-registered sessions (handles /rename) ---
        for session in registry.all() {
            // Only refresh hook sessions (UUID ids), not scanner sessions
            if !session.id.starts_with("scan-") && !session.id.starts_with("window-") {
                if let Some(name) = crate::session::read_transcript_name(&session.id) {
                    if session.session_name.as_deref() != Some(&name) {
                        registry.set_session_name(&session.id, name);
                    }
                }
            }
        }

        status_notify.notify_waiters();
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
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
}
