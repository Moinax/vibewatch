//! Per-agent transcript parsing: find the last assistant text line.

use crate::session::AgentKind;
use std::path::PathBuf;

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
}
