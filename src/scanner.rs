use std::collections::HashSet;
use std::fs;

use crate::compositor::Compositor;
use crate::config::Config;
use crate::session::{detect_terminal, AgentKind, Session, SessionRegistry};

const CLAUDE_CODE_NAMES: &[&str] = &["claude"];
const CODEX_NAMES: &[&str] = &["codex"];

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

        // Read the comm file for the short process name
        let comm_path = format!("/proc/{}/comm", pid);
        let comm = match fs::read_to_string(&comm_path) {
            Ok(c) => c.trim().to_string(),
            Err(_) => continue,
        };

        let comm_lower = comm.to_lowercase();

        if CLAUDE_CODE_NAMES.iter().any(|n| comm_lower == *n) {
            results.push((AgentKind::ClaudeCode, pid));
        } else if CODEX_NAMES.iter().any(|n| comm_lower == *n) {
            results.push((AgentKind::Codex, pid));
        }
    }

    results
}

/// Extract session name from a claude process cmdline.
/// Looks for `--resume <name>` or `--continue <name>` or `-c <name>` patterns.
fn read_session_name_from_cmdline(pid: u32) -> Option<String> {
    let cmdline = fs::read_to_string(format!("/proc/{}/cmdline", pid)).ok()?;
    let args: Vec<&str> = cmdline.split('\0').collect();

    // Look for --resume, --continue, -c followed by a session name
    for i in 0..args.len().saturating_sub(1) {
        match args[i] {
            "--resume" | "--continue" | "-c" => {
                let name = args[i + 1].trim();
                if !name.is_empty() && !name.starts_with('-') {
                    return Some(name.to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Background scanner loop. Runs every 3 seconds, discovering CLI agent
/// processes via /proc and GUI agent windows via the compositor.
pub async fn run_scanner(
    registry: SessionRegistry,
    compositor: Box<dyn Compositor>,
    config: Config,
) {
    loop {
        // Remove sessions whose PID is no longer alive
        registry.cleanup_dead();

        // --- CLI agent scanning ---
        let found_processes = scan_agent_processes();
        let all_sessions = registry.all();
        let known_pids: HashSet<u32> = all_sessions.iter().map(|s| s.pid).collect();

        for (kind, pid) in &found_processes {
            // Skip if any session (hook-registered or scanner) already tracks this PID
            if !known_pids.contains(pid) {
                let id = format!("scan-{}-{}", agent_str(kind), pid);
                let mut session = Session::new(id, *kind, *pid);
                session.session_name = read_session_name_from_cmdline(*pid);
                session.terminal = Some(detect_terminal(*pid));
                registry.register(session);
            }
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
