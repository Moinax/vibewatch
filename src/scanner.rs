use std::collections::HashSet;
use std::fs;

use crate::compositor::Compositor;
use crate::config::Config;
use crate::session::{AgentKind, Session, SessionRegistry};

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

        // --- Refresh session names from transcripts ---
        for session in registry.all() {
            if let Some(transcript) = find_transcript_path(&session.id, session.pid) {
                if let Some(name) = read_session_name_from_transcript(&transcript) {
                    if session.session_name.as_deref() != Some(&name) {
                        registry.set_session_name(&session.id, name);
                    }
                }
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}

/// Find the transcript path for a session.
/// Strategies in order:
/// 1. Hook sessions: session ID is a UUID — search ~/.claude/projects/*/{id}.jsonl
/// 2. All sessions: map cwd → project dir, find most recently modified transcript that
///    contains a matching sessionId being actively written (modified in last 60s)
fn find_transcript_path(session_id: &str, pid: u32) -> Option<String> {
    let claude_projects = dirs::home_dir()?.join(".claude/projects");

    // Strategy 1: session ID is a UUID from hooks
    if session_id.contains('-') && !session_id.starts_with("scan-") && !session_id.starts_with("window-") {
        if let Ok(projects) = fs::read_dir(&claude_projects) {
            for project in projects.flatten() {
                let transcript = project.path().join(format!("{}.jsonl", session_id));
                if transcript.exists() {
                    return Some(transcript.to_string_lossy().to_string());
                }
            }
        }
    }

    // Strategy 2: map cwd to project dir, find active transcript
    let cwd = fs::read_link(format!("/proc/{}/cwd", pid)).ok()?;
    let project_dir_name = cwd.to_string_lossy().replace('/', "-");
    let project_path = claude_projects.join(&project_dir_name);

    if !project_path.exists() {
        return None;
    }

    // Find transcripts modified in the last 60 seconds (actively written to)
    let now = std::time::SystemTime::now();
    let mut candidates: Vec<(std::path::PathBuf, std::time::SystemTime)> = Vec::new();

    if let Ok(entries) = fs::read_dir(&project_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Ok(meta) = path.metadata() {
                if let Ok(modified) = meta.modified() {
                    if let Ok(age) = now.duration_since(modified) {
                        if age.as_secs() < 60 {
                            candidates.push((path, modified));
                        }
                    }
                }
            }
        }
    }

    // Sort by most recent first
    candidates.sort_by(|a, b| b.1.cmp(&a.1));

    // Return the most recently modified active transcript
    candidates.into_iter().next().map(|(p, _)| p.to_string_lossy().to_string())
}

/// Read the session name from a Claude Code transcript file (last custom-title entry).
fn read_session_name_from_transcript(transcript_path: &str) -> Option<String> {
    let content = fs::read_to_string(transcript_path).ok()?;
    for line in content.lines().rev() {
        if line.contains("\"custom-title\"") {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(title) = val.get("customTitle").and_then(|v| v.as_str()) {
                    return Some(title.to_string());
                }
            }
        }
    }
    None
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
