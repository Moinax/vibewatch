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
    match agent {
        AgentKind::Cursor | AgentKind::WebStorm => None,
        AgentKind::ClaudeCode => {
            let home = dirs::home_dir()?;
            read_last_assistant_line_in(agent, &home.join(".claude"), session_id, cached_path)
        }
        AgentKind::Codex => None, // Task 5/6
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

/// Return the last non-empty, non-code-fence-only line of `text`.
fn last_non_empty_line(text: &str) -> Option<String> {
    for line in text.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if is_code_fence(trimmed) {
            continue;
        }
        return Some(trimmed.to_string());
    }
    None
}

fn is_code_fence(trimmed: &str) -> bool {
    trimmed == "```"
        || (trimmed.starts_with("```")
            && !trimmed.contains(' ')
            && trimmed.chars().skip(3).all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
}

/// Parse a Claude JSONL file and return the last non-empty assistant text line.
/// Iterates lines from the end; the first line whose assistant `content` contains
/// at least one text block wins. Returns `None` if no such line exists.
fn parse_claude(content: &str) -> Option<String> {
    for line in content.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let msg = value.get("message").unwrap_or(&value);
        if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }
        let content_arr = match msg.get("content").and_then(|c| c.as_array()) {
            Some(a) => a,
            None => continue,
        };
        let mut joined = String::new();
        for block in content_arr {
            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    if !joined.is_empty() {
                        joined.push('\n');
                    }
                    joined.push_str(text);
                }
            }
        }
        if joined.is_empty() {
            continue;
        }
        if let Some(last) = last_non_empty_line(&joined) {
            return Some(last);
        }
    }
    None
}

/// Testable variant of `read_last_assistant_line` that accepts an explicit
/// `.claude`-equivalent root directory.
pub(crate) fn read_last_assistant_line_in(
    agent: AgentKind,
    root: &Path,
    session_id: &str,
    cached_path: &mut Option<PathBuf>,
) -> Option<String> {
    match agent {
        AgentKind::Cursor | AgentKind::WebStorm => None,
        AgentKind::ClaudeCode => {
            let path = match cached_path {
                Some(p) if p.exists() => p.clone(),
                _ => {
                    let resolved = resolve_claude_path_in(root, session_id)?;
                    *cached_path = Some(resolved.clone());
                    resolved
                }
            };
            let content = std::fs::read_to_string(&path).ok()?;
            parse_claude(&content)
        }
        AgentKind::Codex => None, // Task 5/6
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

    fn claude_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/transcripts/claude")
    }

    #[test]
    fn claude_ends_with_text_returns_last_non_empty_line() {
        let got = read_last_assistant_line_in(
            AgentKind::ClaudeCode,
            &claude_root(),
            "cafe1234-0000-0000-0000-000000000002",
            &mut None,
        );
        assert_eq!(got.as_deref(), Some("Starting now."));
    }

    #[test]
    fn claude_ends_with_tool_use_falls_back_to_earlier_text() {
        let got = read_last_assistant_line_in(
            AgentKind::ClaudeCode,
            &claude_root(),
            "cafe1234-0000-0000-0000-000000000003",
            &mut None,
        );
        // The most recent assistant message is text-less; next-most-recent has text.
        assert_eq!(got.as_deref(), Some("Reading the file."));
    }

    #[test]
    fn claude_multi_text_blocks_concatenates_and_picks_last_line() {
        let got = read_last_assistant_line_in(
            AgentKind::ClaudeCode,
            &claude_root(),
            "cafe1234-0000-0000-0000-000000000004",
            &mut None,
        );
        assert_eq!(got.as_deref(), Some("Second block line D."));
    }

    #[test]
    fn claude_empty_transcript_returns_none() {
        let got = read_last_assistant_line_in(
            AgentKind::ClaudeCode,
            &claude_root(),
            "cafe1234-0000-0000-0000-000000000005",
            &mut None,
        );
        assert!(got.is_none());
    }

    #[test]
    fn claude_malformed_lines_are_skipped() {
        let got = read_last_assistant_line_in(
            AgentKind::ClaudeCode,
            &claude_root(),
            "cafe1234-0000-0000-0000-000000000006",
            &mut None,
        );
        assert_eq!(got.as_deref(), Some("Malformed-resistant answer."));
    }

    #[test]
    fn claude_trailing_code_fence_is_stripped() {
        let got = read_last_assistant_line_in(
            AgentKind::ClaudeCode,
            &claude_root(),
            "cafe1234-0000-0000-0000-000000000007",
            &mut None,
        );
        // Last non-empty, non-fence line is the code content before the fence.
        assert_eq!(got.as_deref(), Some("let x = 1;"));
    }

    #[test]
    fn claude_cached_path_is_populated_on_success() {
        let mut cache = None;
        let _ = read_last_assistant_line_in(
            AgentKind::ClaudeCode,
            &claude_root(),
            "cafe1234-0000-0000-0000-000000000002",
            &mut cache,
        );
        assert!(cache.is_some());
        // Second call hits the cache — works even if the filesystem search would fail.
        let still = read_last_assistant_line_in(
            AgentKind::ClaudeCode,
            &claude_root().join("does_not_exist"),
            "ignored",
            &mut cache,
        );
        assert_eq!(still.as_deref(), Some("Starting now."));
    }
}
