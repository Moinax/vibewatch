# Description-line last-sentence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the session-row description line always show the most recent line spoken in the session — user prompt or agent assistant text — with a speaker prefix, so the panel never falls back to a generic "Idle"/"Thinking..." when there's real content to display.

**Architecture:** On every Claude/Codex hook event, the daemon reads the last assistant text line from that session's transcript JSONL and stores it on the in-memory `Session`. A pure render function on `Session` picks whichever of `last_prompt` / `last_agent_text` is more recent and renders it with a `You:` / `Claude:` / `Codex:` prefix. Cursor and WebStorm gracefully fall back to status text (no accessible transcript).

**Tech Stack:** Rust 2021, `serde_json` (already a dep), `dirs` (already a dep), `tokio` (existing). No new crates.

**Related spec:** `docs/superpowers/specs/2026-04-17-description-line-last-sentence-design.md` (commit `39b0f59`).

---

## File Structure

**New files:**

| Path | Responsibility |
|---|---|
| `src/transcript.rs` | Per-agent transcript path resolution + parsing of the last assistant text line. Single public entry point: `read_last_assistant_line`. |
| `tests/fixtures/transcripts/claude/assistant_ends_with_text.jsonl` | Claude transcript whose last assistant message has a final text block. |
| `tests/fixtures/transcripts/claude/assistant_ends_with_tool_use.jsonl` | Claude transcript whose last assistant message ends with a tool_use (no text block). |
| `tests/fixtures/transcripts/claude/multi_text_blocks.jsonl` | Claude transcript whose last assistant message has several text blocks. |
| `tests/fixtures/transcripts/claude/empty.jsonl` | Empty file. |
| `tests/fixtures/transcripts/claude/malformed_lines.jsonl` | Mixes malformed JSON with valid lines. |
| `tests/fixtures/transcripts/claude/ends_with_code_fence.jsonl` | Last assistant text block ends with a code fence line. |
| `tests/fixtures/transcripts/codex/assistant_ends_with_text.jsonl` | Codex analogue. |
| `tests/fixtures/transcripts/codex/empty.jsonl` | Empty file. |
| `tests/fixtures/transcripts/codex/malformed_lines.jsonl` | Codex analogue. |

**Modified files:**

| Path | Change |
|---|---|
| `src/session.rs` | Add `last_agent_text`, `last_agent_text_at`, `last_prompt_at`, `transcript_path` fields to `Session` and initialize them. |
| `src/lib.rs` | `pub mod transcript;` |
| `src/main.rs` | Add `mod transcript;`, set `last_prompt_at` on `UserPromptSubmit`, call `transcript::read_last_assistant_line` on `PostToolUse` and `Stop`. |
| `src/panel/session_row.rs` | Replace `status_description` with pure `describe()`; extend `action_line` to handle `WaitingApproval`. Add unit tests. |

---

## Shared Types & Signatures

These names appear in multiple tasks; keep them consistent:

```rust
// Session fields (session.rs)
pub last_agent_text:   Option<String>,
pub last_agent_text_at: Option<u64>,   // unix epoch seconds
pub last_prompt_at:     Option<u64>,    // unix epoch seconds
pub transcript_path:    Option<std::path::PathBuf>,

// transcript.rs public API
pub fn read_last_assistant_line(
    agent: crate::session::AgentKind,
    session_id: &str,
    cached_path: &mut Option<std::path::PathBuf>,
) -> Option<String>;

// session_row.rs internal (non-pub)
fn describe(session: &crate::session::Session) -> String;
fn action_line(session: &crate::session::Session) -> Option<String>;  // extended
```

Helper: the current UNIX epoch helper lives inline in `main.rs` as `std::time::SystemTime::now().duration_since(UNIX_EPOCH)...`. Reuse the existing pattern (see `session.rs` line 105).

---

## Task 1: Add new fields to `Session`

**Files:**
- Modify: `src/session.rs` (struct around lines 70-87, `Session::new` around lines 89-110)
- Modify: `src/session.rs` (tests module around line 395)

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `src/session.rs` (just before the closing `}` of `mod tests`):

```rust
#[test]
fn new_session_has_null_agent_and_prompt_timestamps() {
    let s = Session::new("s1".into(), AgentKind::ClaudeCode, 42);
    assert!(s.last_agent_text.is_none());
    assert!(s.last_agent_text_at.is_none());
    assert!(s.last_prompt_at.is_none());
    assert!(s.transcript_path.is_none());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib session::tests::new_session_has_null_agent_and_prompt_timestamps`

Expected: FAIL with compilation error about missing fields `last_agent_text`, etc.

- [ ] **Step 3: Add fields to `Session` struct**

In `src/session.rs`, extend the struct (the existing fields stay; add these four at the bottom before the closing brace):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub agent: AgentKind,
    pub status: SessionStatus,
    pub current_tool: Option<String>,
    pub tool_detail: Option<String>,
    pub last_tool: Option<String>,
    pub last_tool_detail: Option<String>,
    pub last_prompt: Option<String>,
    pub session_name: Option<String>,
    pub window_id: Option<String>,
    pub cwd: Option<String>,
    pub terminal: Option<String>,
    pub pid: u32,
    /// Unix epoch seconds when session was first seen
    pub started_at_epoch: Option<u64>,
    /// Last assistant text line read from the transcript (Claude/Codex only).
    #[serde(default)]
    pub last_agent_text: Option<String>,
    /// Unix epoch seconds when `last_agent_text` was last updated.
    #[serde(default)]
    pub last_agent_text_at: Option<u64>,
    /// Unix epoch seconds when `last_prompt` was last set.
    #[serde(default)]
    pub last_prompt_at: Option<u64>,
    /// Cached path to the transcript file once resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<std::path::PathBuf>,
}
```

- [ ] **Step 4: Initialize new fields in `Session::new`**

Modify the `Session::new` constructor in the same file to initialize the new fields to `None`:

```rust
impl Session {
    pub fn new(id: String, agent: AgentKind, pid: u32) -> Self {
        Self {
            id,
            agent,
            status: SessionStatus::Idle,
            current_tool: None,
            tool_detail: None,
            last_tool: None,
            last_tool_detail: None,
            last_prompt: None,
            session_name: None,
            window_id: None,
            cwd: None,
            terminal: None,
            pid,
            started_at_epoch: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs()),
            last_agent_text: None,
            last_agent_text_at: None,
            last_prompt_at: None,
            transcript_path: None,
        }
    }
    // ... rest of impl unchanged
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib session::tests`

Expected: all session tests pass, including the new one.

- [ ] **Step 6: Commit**

```bash
git add src/session.rs
git commit -m "session: add last_agent_text, last_prompt_at, transcript_path fields"
```

---

## Task 2: Scaffold the `transcript` module

**Files:**
- Create: `src/transcript.rs`
- Modify: `src/lib.rs` (add `pub mod transcript;`)
- Modify: `src/main.rs` (add `mod transcript;`)

- [ ] **Step 1: Write the failing test**

Create `src/transcript.rs` with a failing test that exercises the public API on `Cursor`/`WebStorm`:

```rust
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
```

- [ ] **Step 2: Register the module**

In `src/lib.rs` add after `pub mod session;`:

```rust
pub mod transcript;
```

In `src/main.rs` add after `mod session;`:

```rust
mod transcript;
```

- [ ] **Step 3: Run the test**

Run: `cargo test --lib transcript::`

Expected: PASS (one test).

- [ ] **Step 4: Commit**

```bash
git add src/transcript.rs src/lib.rs src/main.rs
git commit -m "transcript: scaffold module with Cursor/WebStorm no-op"
```

---

## Task 3: Claude transcript path resolution

**Files:**
- Create: `tests/fixtures/transcripts/claude/projects/-test-project/cafe1234-0000-0000-0000-000000000001.jsonl`
- Modify: `src/transcript.rs`

The production path search uses `dirs::home_dir()` + `.claude/projects/*`. For tests we'll introduce a helper that takes a root dir so we can point it at a fixture tree.

- [ ] **Step 1: Create the fixture file**

```bash
mkdir -p tests/fixtures/transcripts/claude/projects/-test-project
```

Write `tests/fixtures/transcripts/claude/projects/-test-project/cafe1234-0000-0000-0000-000000000001.jsonl` with a single valid line (content doesn't matter for path resolution):

```json
{"type":"user","message":{"role":"user","content":"hi"}}
```

- [ ] **Step 2: Write the failing test**

Append to `#[cfg(test)] mod tests` in `src/transcript.rs`:

```rust
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
```

- [ ] **Step 3: Run tests to confirm failure**

Run: `cargo test --lib transcript::tests::claude_path`

Expected: FAIL (function `resolve_claude_path_in` not defined).

- [ ] **Step 4: Implement path resolution**

Append to `src/transcript.rs` (above `mod tests`):

```rust
use std::path::Path;

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
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib transcript::`

Expected: PASS (3 tests now).

- [ ] **Step 6: Commit**

```bash
git add src/transcript.rs tests/fixtures/transcripts/claude
git commit -m "transcript: resolve Claude transcript path from ~/.claude/projects/*"
```

---

## Task 4: Claude last-assistant-text parser

**Files:**
- Create: `tests/fixtures/transcripts/claude/projects/-test-project/cafe1234-0000-0000-0000-000000000002.jsonl` (ends with text)
- Create: `tests/fixtures/transcripts/claude/projects/-test-project/cafe1234-0000-0000-0000-000000000003.jsonl` (ends with tool_use)
- Create: `tests/fixtures/transcripts/claude/projects/-test-project/cafe1234-0000-0000-0000-000000000004.jsonl` (multi text blocks)
- Create: `tests/fixtures/transcripts/claude/projects/-test-project/cafe1234-0000-0000-0000-000000000005.jsonl` (empty)
- Create: `tests/fixtures/transcripts/claude/projects/-test-project/cafe1234-0000-0000-0000-000000000006.jsonl` (malformed)
- Create: `tests/fixtures/transcripts/claude/projects/-test-project/cafe1234-0000-0000-0000-000000000007.jsonl` (ends with code fence)
- Modify: `src/transcript.rs`

- [ ] **Step 1: Write the fixture files**

Write each of the files below exactly (each line is ONE jsonl record; keep on one physical line):

`cafe1234-0000-0000-0000-000000000002.jsonl` (ends with text):

```
{"type":"user","message":{"role":"user","content":"do thing"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Sure. I'll do the thing.\nStarting now."}]}}
```

`cafe1234-0000-0000-0000-000000000003.jsonl` (ends with tool_use):

```
{"type":"user","message":{"role":"user","content":"do thing"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Reading the file."},{"type":"tool_use","name":"Read","input":{"file":"src/main.rs"}}]}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"..."}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Edit","input":{}}]}}
```

`cafe1234-0000-0000-0000-000000000004.jsonl` (multi text blocks):

```
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"First block line A.\nFirst block line B."},{"type":"thinking","thinking":"..."},{"type":"text","text":"Second block line C.\nSecond block line D."}]}}
```

`cafe1234-0000-0000-0000-000000000005.jsonl` (empty): create an empty file.

`cafe1234-0000-0000-0000-000000000006.jsonl` (malformed lines):

```
{not valid json
{"type":"user","message":{"role":"user","content":"hi"}}
also not json
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Malformed-resistant answer."}]}}
garbage
```

`cafe1234-0000-0000-0000-000000000007.jsonl` (ends with code fence, so last non-empty line is the fence):

```
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Here is the snippet:\n```rust\nlet x = 1;\n```"}]}}
```

- [ ] **Step 2: Write failing tests**

Append to `#[cfg(test)] mod tests` in `src/transcript.rs`:

```rust
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
```

- [ ] **Step 3: Run tests to confirm failure**

Run: `cargo test --lib transcript::tests::claude_`

Expected: FAIL — `read_last_assistant_line_in`, `parse_claude`, `last_non_empty_line` not defined.

- [ ] **Step 4: Implement helpers**

Append to `src/transcript.rs` (above `mod tests`):

```rust
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
    trimmed == "```" || trimmed.starts_with("```")
        && !trimmed.contains(' ')
        && trimmed.chars().skip(3).all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
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
```

- [ ] **Step 5: Rewire the production entry point**

Update `read_last_assistant_line` (the existing public fn) to delegate to `read_last_assistant_line_in` with the real `~/.claude` root:

```rust
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
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --lib transcript::`

Expected: PASS (all Claude tests + pre-existing Cursor/WebStorm + path resolution tests).

- [ ] **Step 7: Commit**

```bash
git add src/transcript.rs tests/fixtures/transcripts/claude
git commit -m "transcript: parse last assistant text line from Claude JSONL"
```

---

## Task 5: Codex transcript path resolution

Codex stores sessions at `~/.codex/sessions/<yyyy>/<mm>/<dd>/rollout-...-<session_id>.jsonl`. Search is a recursive walk matching the filename suffix `-<session_id>.jsonl`.

**Files:**
- Create: `tests/fixtures/transcripts/codex/sessions/2026/04/17/rollout-2026-04-17T10-00-00-codex0001-0000-0000-0000-000000000001.jsonl`
- Modify: `src/transcript.rs`

- [ ] **Step 1: Create the fixture**

```bash
mkdir -p tests/fixtures/transcripts/codex/sessions/2026/04/17
```

Write `tests/fixtures/transcripts/codex/sessions/2026/04/17/rollout-2026-04-17T10-00-00-codex0001-0000-0000-0000-000000000001.jsonl` with a single session_meta line:

```
{"timestamp":"2026-04-17T10:00:01Z","type":"session_meta","payload":{"id":"codex0001-0000-0000-0000-000000000001","cwd":"/tmp"}}
```

- [ ] **Step 2: Write failing tests**

Append to `mod tests`:

```rust
    fn codex_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/transcripts/codex")
    }

    #[test]
    fn codex_path_found_by_recursive_walk() {
        let got = resolve_codex_path_in(
            &codex_root(),
            "codex0001-0000-0000-0000-000000000001",
        );
        assert!(got.is_some(), "expected path resolution to succeed");
        let p = got.unwrap();
        assert!(p.to_string_lossy().ends_with(
            "codex0001-0000-0000-0000-000000000001.jsonl"
        ));
    }

    #[test]
    fn codex_path_none_for_unknown_session() {
        let got = resolve_codex_path_in(&codex_root(), "nope");
        assert!(got.is_none());
    }
```

- [ ] **Step 3: Run tests to confirm failure**

Run: `cargo test --lib transcript::tests::codex_path`

Expected: FAIL — function not defined.

- [ ] **Step 4: Implement Codex path resolution**

Append to `src/transcript.rs` (above `mod tests`):

```rust
/// Walk `<root>/sessions` recursively for a file named `*-<session_id>.jsonl`.
fn resolve_codex_path_in(root: &Path, session_id: &str) -> Option<PathBuf> {
    let sessions = root.join("sessions");
    let suffix = format!("-{}.jsonl", session_id);
    walk_for_suffix(&sessions, &suffix)
}

fn walk_for_suffix(dir: &Path, suffix: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = walk_for_suffix(&path, suffix) {
                return Some(found);
            }
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.ends_with(suffix))
            .unwrap_or(false)
        {
            return Some(path);
        }
    }
    None
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib transcript::`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/transcript.rs tests/fixtures/transcripts/codex
git commit -m "transcript: resolve Codex transcript path via recursive walk"
```

---

## Task 6: Codex last-assistant-text parser

**Files:**
- Create: `tests/fixtures/transcripts/codex/sessions/2026/04/17/rollout-2026-04-17T10-01-00-codex0002-0000-0000-0000-000000000002.jsonl` (ends with text)
- Create: `tests/fixtures/transcripts/codex/sessions/2026/04/17/rollout-2026-04-17T10-02-00-codex0003-0000-0000-0000-000000000003.jsonl` (empty)
- Create: `tests/fixtures/transcripts/codex/sessions/2026/04/17/rollout-2026-04-17T10-03-00-codex0004-0000-0000-0000-000000000004.jsonl` (malformed lines)
- Modify: `src/transcript.rs`

- [ ] **Step 1: Create fixtures**

`...codex0002-...0002.jsonl`:

```
{"timestamp":"2026-04-17T10:01:01Z","type":"session_meta","payload":{"id":"codex0002-0000-0000-0000-000000000002"}}
{"timestamp":"2026-04-17T10:01:02Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}
{"timestamp":"2026-04-17T10:01:03Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Working on it.\nAll set."}],"phase":"final_answer"}}
```

`...codex0003-...0003.jsonl`: empty file.

`...codex0004-...0004.jsonl`:

```
{not json
{"timestamp":"2026-04-17T10:03:01Z","type":"session_meta","payload":{"id":"codex0004-0000-0000-0000-000000000004"}}
garbage line
{"timestamp":"2026-04-17T10:03:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Survived malformed lines."}]}}
also bad
```

- [ ] **Step 2: Write failing tests**

Append to `mod tests`:

```rust
    #[test]
    fn codex_ends_with_text_returns_last_non_empty_line() {
        let got = read_last_assistant_line_in(
            AgentKind::Codex,
            &codex_root(),
            "codex0002-0000-0000-0000-000000000002",
            &mut None,
        );
        assert_eq!(got.as_deref(), Some("All set."));
    }

    #[test]
    fn codex_empty_returns_none() {
        let got = read_last_assistant_line_in(
            AgentKind::Codex,
            &codex_root(),
            "codex0003-0000-0000-0000-000000000003",
            &mut None,
        );
        assert!(got.is_none());
    }

    #[test]
    fn codex_malformed_lines_are_skipped() {
        let got = read_last_assistant_line_in(
            AgentKind::Codex,
            &codex_root(),
            "codex0004-0000-0000-0000-000000000004",
            &mut None,
        );
        assert_eq!(got.as_deref(), Some("Survived malformed lines."));
    }
```

- [ ] **Step 3: Run tests to confirm failure**

Run: `cargo test --lib transcript::tests::codex_`

Expected: FAIL — Codex branch in `read_last_assistant_line_in` still returns `None`.

- [ ] **Step 4: Implement the Codex parser and wire it up**

Append to `src/transcript.rs` (above `mod tests`):

```rust
/// Parse a Codex JSONL file and return the last non-empty assistant text line.
fn parse_codex(content: &str) -> Option<String> {
    for line in content.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if value.get("type").and_then(|t| t.as_str()) != Some("response_item") {
            continue;
        }
        let payload = match value.get("payload") {
            Some(p) => p,
            None => continue,
        };
        if payload.get("type").and_then(|t| t.as_str()) != Some("message")
            || payload.get("role").and_then(|r| r.as_str()) != Some("assistant")
        {
            continue;
        }
        let content_arr = match payload.get("content").and_then(|c| c.as_array()) {
            Some(a) => a,
            None => continue,
        };
        let mut joined = String::new();
        for block in content_arr {
            if block.get("type").and_then(|t| t.as_str()) == Some("output_text") {
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
```

Then extend the Codex branch in `read_last_assistant_line_in`:

```rust
        AgentKind::Codex => {
            let path = match cached_path {
                Some(p) if p.exists() => p.clone(),
                _ => {
                    let resolved = resolve_codex_path_in(root, session_id)?;
                    *cached_path = Some(resolved.clone());
                    resolved
                }
            };
            let content = std::fs::read_to_string(&path).ok()?;
            parse_codex(&content)
        }
```

And the production entry point:

```rust
        AgentKind::Codex => {
            let home = dirs::home_dir()?;
            read_last_assistant_line_in(agent, &home.join(".codex"), session_id, cached_path)
        }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib transcript::`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/transcript.rs tests/fixtures/transcripts/codex
git commit -m "transcript: parse last assistant text line from Codex JSONL"
```

---

## Task 7: Wire transcript reads into `main.rs` event handlers

**Files:**
- Modify: `src/main.rs` around lines 267-306 (the `UserPromptSubmit`, `PostToolUse`, `Stop` handlers)

- [ ] **Step 1: Add a local helper for "now"**

Near `fn get_session(...)` in `src/main.rs` (around line 326), add:

```rust
fn now_epoch() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}
```

- [ ] **Step 2: Update `UserPromptSubmit` to set the timestamp**

In `src/main.rs`, replace the `UserPromptSubmit` block (around lines 267-279):

```rust
            InboundEvent::UserPromptSubmit { session_id, prompt } => {
                if let Some(mut session) = get_session(&registry, &session_id) {
                    session.status = SessionStatus::Thinking;
                    session.last_prompt = prompt;
                    session.last_prompt_at = now_epoch();
                    session.current_tool = None;
                    session.tool_detail = None;
                    if let Some(name) = session::read_transcript_name(&session_id) {
                        session.session_name = Some(name);
                    }
                    session.touch();
                    registry.register(session);
                }
            }
```

- [ ] **Step 3: Update `PostToolUse` to read transcript after the tool completes**

Replace the `PostToolUse` block (around lines 251-266):

```rust
            InboundEvent::PostToolUse {
                session_id,
                tool: _,
                success,
            } => {
                if let Some(mut session) = get_session(&registry, &session_id) {
                    session.last_tool = session.current_tool.take();
                    session.last_tool_detail = session.tool_detail.take();
                    session.status = SessionStatus::Thinking;
                    let agent = session.agent;
                    if let Some(text) = transcript::read_last_assistant_line(
                        agent,
                        &session_id,
                        &mut session.transcript_path,
                    ) {
                        session.last_agent_text = Some(text);
                        session.last_agent_text_at = now_epoch();
                    }
                    session.touch();
                    registry.register(session);
                }
                if !success {
                    sound_player.play(SoundEvent::Error);
                }
            }
```

- [ ] **Step 4: Update `Stop` similarly**

Replace the `Stop` block (around lines 298-306):

```rust
            InboundEvent::Stop { session_id } => {
                if let Some(mut session) = registry.get(&session_id) {
                    session.status = SessionStatus::Idle;
                    session.current_tool = None;
                    session.tool_detail = None;
                    let agent = session.agent;
                    if let Some(text) = transcript::read_last_assistant_line(
                        agent,
                        &session_id,
                        &mut session.transcript_path,
                    ) {
                        session.last_agent_text = Some(text);
                        session.last_agent_text_at = now_epoch();
                    }
                    session.touch();
                    registry.register(session);
                }
            }
```

- [ ] **Step 5: Build and run all existing tests**

Run: `cargo build && cargo test`

Expected: build succeeds; all tests pass. Transcript reads are best-effort — they won't fire against fixture files during unit tests (they'd need `$HOME` set), but the production code paths compile and `Session`-level tests still pass.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "daemon: capture last_agent_text on PostToolUse and Stop"
```

---

## Task 8: Pure `describe()` render function with tests

**Files:**
- Modify: `src/panel/session_row.rs`

The existing code guards the panel with `#[cfg(feature = "panel")]` via `mod panel;`. Tests in `session_row.rs` will therefore only run when the `panel` feature is enabled (the default). To unit-test `describe()` without GTK, keep it a pure function operating on `&Session` only, no widget access.

- [ ] **Step 1: Write the failing tests**

Append to `src/panel/session_row.rs` at the end of the file:

```rust
#[cfg(test)]
mod tests {
    use super::describe;
    use crate::session::{AgentKind, Session, SessionStatus};

    fn mk(agent: AgentKind) -> Session {
        Session::new("s1".into(), agent, 1)
    }

    #[test]
    fn user_only_renders_you_prefix() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.last_prompt = Some("fix the deploy".into());
        s.last_prompt_at = Some(100);
        assert_eq!(describe(&s), "You: \"fix the deploy\"");
    }

    #[test]
    fn agent_only_renders_agent_prefix() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.last_agent_text = Some("Tests pass.".into());
        s.last_agent_text_at = Some(100);
        assert_eq!(describe(&s), "Claude: \"Tests pass.\"");
    }

    #[test]
    fn codex_agent_uses_codex_prefix() {
        let mut s = mk(AgentKind::Codex);
        s.last_agent_text = Some("All good.".into());
        s.last_agent_text_at = Some(100);
        assert_eq!(describe(&s), "Codex: \"All good.\"");
    }

    #[test]
    fn user_wins_when_newer() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.last_prompt = Some("please do X".into());
        s.last_prompt_at = Some(200);
        s.last_agent_text = Some("done".into());
        s.last_agent_text_at = Some(100);
        assert_eq!(describe(&s), "You: \"please do X\"");
    }

    #[test]
    fn agent_wins_when_newer_or_equal() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.last_prompt = Some("please do X".into());
        s.last_prompt_at = Some(100);
        s.last_agent_text = Some("done".into());
        s.last_agent_text_at = Some(100);
        assert_eq!(describe(&s), "Claude: \"done\"");
    }

    #[test]
    fn idle_fallback_when_nothing_captured() {
        let s = mk(AgentKind::ClaudeCode);
        assert_eq!(describe(&s), "Idle");
    }

    #[test]
    fn stopped_fallback() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.status = SessionStatus::Stopped;
        assert_eq!(describe(&s), "Stopped");
    }

    #[test]
    fn executing_fallback_when_no_text() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.status = SessionStatus::Executing;
        assert_eq!(describe(&s), "Working...");
    }

    #[test]
    fn waiting_approval_fallback_when_no_text() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.status = SessionStatus::WaitingApproval;
        assert_eq!(describe(&s), "Awaiting approval");
    }

    #[test]
    fn long_text_is_truncated_with_ellipsis() {
        let mut s = mk(AgentKind::ClaudeCode);
        let long: String = "x".repeat(200);
        s.last_prompt = Some(long.clone());
        s.last_prompt_at = Some(1);
        let out = describe(&s);
        assert!(out.starts_with("You: \""));
        assert!(out.ends_with("...\""));
        assert!(out.len() < long.len() + 20);
    }
}
```

- [ ] **Step 2: Run the tests to confirm failure**

Run: `cargo test panel::session_row::tests`

(Note: `panel` lives in the binary crate, not the library — don't pass `--lib`.)

Expected: FAIL — `describe` not defined.

- [ ] **Step 3: Implement `describe`**

Add the helpers below to `src/panel/session_row.rs`, above the existing `status_description` function (which will remain unused for now and be removed in Task 9):

```rust
/// Maximum characters of prompt/agent text to render before ellipsizing.
const DESCRIBE_MAX_CHARS: usize = 60;

/// Description-line content for a session: latest of user prompt / agent text,
/// with a speaker prefix. Falls back to a status-based string when neither
/// text is available.
pub(crate) fn describe(session: &Session) -> String {
    let user = session.last_prompt.as_deref().zip(session.last_prompt_at);
    let agent = session.last_agent_text.as_deref().zip(session.last_agent_text_at);
    match (user, agent) {
        (Some((p, _)), None) => render_user(p),
        (None, Some((a, _))) => render_agent(session, a),
        (Some((p, pu)), Some((a, au))) => {
            if pu > au {
                render_user(p)
            } else {
                render_agent(session, a)
            }
        }
        (None, None) => fallback_for_status(session.status),
    }
}

fn render_user(text: &str) -> String {
    format!("You: \"{}\"", truncate(text, DESCRIBE_MAX_CHARS))
}

fn render_agent(session: &Session, text: &str) -> String {
    format!(
        "{}: \"{}\"",
        session.agent.short_name(),
        truncate(text, DESCRIBE_MAX_CHARS),
    )
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(3)).collect();
        out.push_str("...");
        out
    }
}

fn fallback_for_status(status: SessionStatus) -> String {
    match status {
        SessionStatus::Idle | SessionStatus::Running => "Idle".into(),
        SessionStatus::Stopped => "Stopped".into(),
        SessionStatus::Thinking => "Thinking...".into(),
        SessionStatus::Executing => "Working...".into(),
        SessionStatus::WaitingApproval => "Awaiting approval".into(),
    }
}
```

Add the import at the top of the file (if not already present):

```rust
use crate::session::{Session, SessionStatus};
```

(The file currently imports via the `crate::session::{describe_tool, parent_pid, Session, SessionStatus}` line — extend it rather than duplicating.)

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test panel::session_row::tests`

Expected: PASS (all 10 tests).

- [ ] **Step 5: Commit**

```bash
git add src/panel/session_row.rs
git commit -m "panel: pure describe() render function with latest-wins logic"
```

---

## Task 9: Swap `describe()` into the widget and extend `action_line` for WaitingApproval

**Files:**
- Modify: `src/panel/session_row.rs`

- [ ] **Step 1: Write a failing test for `action_line` on `WaitingApproval`**

Append to the `tests` module in `src/panel/session_row.rs`:

```rust
    #[test]
    fn action_line_shows_needs_approval_for_waiting() {
        use super::action_line;
        let mut s = mk(AgentKind::ClaudeCode);
        s.status = SessionStatus::WaitingApproval;
        s.current_tool = Some("Bash".into());
        assert_eq!(action_line(&s).as_deref(), Some("Needs approval: Bash"));
    }

    #[test]
    fn action_line_still_shows_live_tool_when_executing() {
        use super::action_line;
        let mut s = mk(AgentKind::ClaudeCode);
        s.status = SessionStatus::Executing;
        s.current_tool = Some("Edit".into());
        s.tool_detail = Some("src/main.rs".into());
        assert_eq!(action_line(&s).as_deref(), Some("Editing src/main.rs"));
    }
```

- [ ] **Step 2: Run tests to confirm the new approval test fails**

Run: `cargo test panel::session_row::tests::action_line`

Expected: FAIL on `action_line_shows_needs_approval_for_waiting` (current `action_line` returns `None` for `WaitingApproval` without `tool_detail`).

- [ ] **Step 3: Extend `action_line` and swap `describe` into `build_row`**

In `src/panel/session_row.rs`, update `action_line` to handle `WaitingApproval` first:

```rust
fn action_line(session: &Session) -> Option<String> {
    if session.status == SessionStatus::WaitingApproval {
        let tool = session.current_tool.as_deref().unwrap_or("tool");
        return Some(format!("Needs approval: {}", tool));
    }
    if let (Some(tool), Some(detail)) = (&session.current_tool, &session.tool_detail) {
        return Some(describe_tool(tool, detail, true));
    }
    if session.status == SessionStatus::Thinking {
        if let (Some(tool), Some(detail)) = (&session.last_tool, &session.last_tool_detail) {
            return Some(describe_tool(tool, detail, false));
        }
    }
    None
}
```

In `build_row`, replace the `status_description(session)` call with `describe(session)`:

```rust
    let desc_label = gtk::Label::new(Some(&describe(session)));
```

Delete the now-unused `status_description` function (the whole `fn status_description(session: &Session) -> String { ... }` block).

- [ ] **Step 4: Run all tests**

Run: `cargo test`

Expected: PASS for the whole suite.

- [ ] **Step 5: Build the panel binary to catch GTK-related breakage**

Run: `cargo build`

Expected: clean build (no warnings from dead code, since we deleted `status_description`).

- [ ] **Step 6: Commit**

```bash
git add src/panel/session_row.rs
git commit -m "panel: render describe() on description line; move approval to action line"
```

---

## Task 10: Manual verification

**Files:** none modified; this task only runs the daemon and confirms rendering.

- [ ] **Step 1: Build in release mode**

Run: `cargo build --release`

Expected: successful build.

- [ ] **Step 2: Launch the daemon in a Wayland session**

Run: `./target/release/vibewatch daemon`

Leave it running in one terminal.

- [ ] **Step 3: Start a Claude Code session and send a prompt**

In another terminal, with the Claude Code hooks configured per README: start `claude`, send a prompt that triggers a tool (e.g. "read main.rs and tell me what it does").

- [ ] **Step 4: Open the vibewatch panel and confirm description line behavior**

Toggle the panel (via the Waybar module or `./target/release/vibewatch toggle-panel`).

Verify:
- Immediately after sending the prompt: description shows `You: "..."`.
- After Claude runs a tool and replies: description flips to `Claude: "..."` with the last non-empty line of the response.
- When Claude is executing a tool: action line (line 2) shows `Editing foo.rs` / `Reading bar.rs` / etc., while the description line retains the most recent text.
- On an approval prompt: action line shows `Needs approval: Bash`; description still shows the most recent sentence.

If any of the above fails, open an issue or patch in a follow-up commit — do **not** modify this plan.

- [ ] **Step 5: Commit a brief note to the plan if anything was found**

If manual verification passed cleanly, skip this step. Otherwise, append a **"Manual verification notes"** subsection to this plan file with the discrepancy and commit:

```bash
git add docs/superpowers/plans/2026-04-17-description-line-last-sentence.md
git commit -m "plan: note manual verification results"
```

---

## Out of scope / follow-ups

- Bounded reverse-read of transcript files (currently the parser reads the full file each hook). If this becomes a hotspot for large sessions, add a helper that reads the last 256 KiB, locates the first full newline, and parses forward.
- inotify-based streaming updates (the v1 design explicitly rejected this; revisit only if hook-driven refresh feels too stale).
- Better handling of responses that end with bullet lists or tables — the current "last non-empty line" rule is deliberately dumb.
- Emoji/unicode-aware truncation that preserves grapheme clusters (today `truncate` counts `chars`, which is good enough for ASCII/simple unicode but could split a flag emoji).
