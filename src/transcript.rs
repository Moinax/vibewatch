//! Per-agent transcript parsing: find the last assistant text line.

use crate::session::AgentKind;
use std::path::{Path, PathBuf};

/// Read the last assistant text line from the session's transcript file.
///
/// Returns `None` for agents without an accessible transcript (Cursor, WebStorm),
/// if the file cannot be located, if it contains no assistant text, or if the
/// final text line is empty or is only a code fence.
///
/// `cached_path` is used to avoid re-walking the filesystem on every call; on a
/// successful read it is populated with the resolved path.
pub fn read_last_assistant_line(
    agent: AgentKind,
    session_id: &str,
    cached_path: &mut Option<PathBuf>,
) -> Option<String> {
    let _ = (session_id, cached_path);
    match agent {
        AgentKind::Cursor | AgentKind::WebStorm => None,
        AgentKind::ClaudeCode => None, // implemented in Task 3/4
        AgentKind::Codex => None,      // implemented in Task 5/6
    }
}

/// Walk `<root>/projects/*/` looking for `<session_id>.jsonl`.
fn resolve_claude_path_in(root: &Path, session_id: &str) -> Option<PathBuf> {
    let projects = root.join("projects");
    for project in std::fs::read_dir(&projects).ok()?.flatten() {
        let candidate = project.path().join(format!("{}.jsonl", session_id));
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Production entry point that uses `~/.claude`.
fn resolve_claude_path(session_id: &str) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    resolve_claude_path_in(&home.join(".claude"), session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_and_webstorm_return_none() {
        let mut p = None;
        assert!(read_last_assistant_line(AgentKind::Cursor, "s1", &mut p).is_none());
        assert!(read_last_assistant_line(AgentKind::WebStorm, "s1", &mut p).is_none());
        assert!(p.is_none());
    }

    #[test]
    fn claude_path_found_for_known_session() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/transcripts/claude");
        let id = "cafe1234-0000-0000-0000-000000000001";
        let path = resolve_claude_path_in(&root, id).expect("path resolves");
        assert!(path.ends_with("-test-project/cafe1234-0000-0000-0000-000000000001.jsonl"));
    }

    #[test]
    fn claude_path_none_for_unknown_session() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/transcripts/claude");
        assert!(resolve_claude_path_in(&root, "nonexistent-id").is_none());
    }
}
