# Widget Approval UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Accept or deny Claude Code tool-approval requests directly from the vibewatch panel (Accept / Deny buttons on the session row), auto-showing the panel when one arrives and falling back to Claude's terminal prompt if the user never clicks.

**Architecture:** The existing fire-and-forget `permission-request` hook becomes a **blocking** hook: it opens a Unix-socket connection to the daemon, sends a `PermissionRequest` event that carries a fresh `request_id` + tool + detail, keeps the connection open, and blocks reading a single JSON decision line with a 580 s timeout. The daemon moves the hook's write half into a new `ApprovalRegistry`, fills `session.pending_approval`, and auto-shows the panel. When the user clicks Accept or Deny, the panel sends an `ApprovalDecision { request_id, approved }` event on a **new** socket connection; the daemon takes the registry entry, writes `{"approved":true|false}` back on the stashed stream, clears `pending_approval`, and returns. The hook reads the line, writes Claude's `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","permissionDecision":"allow"|"deny"}}` to stdout, and exits.

**Tech Stack:** Rust 2021, `tokio` (existing), `serde_json` (existing), `gtk4` + `libadwaita` (existing). No new crates.

**Related spec:** `docs/superpowers/specs/2026-04-17-widget-approval-ui-design.md` (commit `0052f50`).

---

## File Structure

**New files:**

| Path | Responsibility |
|---|---|
| `src/approval.rs` | `PendingApproval` consumer-side + `ApprovalRegistry` type that owns the held hook `OwnedWriteHalf`s keyed by `request_id`. One public API: `new`, `insert`, `take`, `reap_stale`. |

**Modified files:**

| Path | Change |
|---|---|
| `src/session.rs` | Add `PendingApproval` struct + `pending_approval: Option<PendingApproval>` field + initializer. Move `PendingApproval` here so `Session` can carry it serializable-side-only (stream lives in `approval.rs`). |
| `src/ipc.rs` | Extend `InboundEvent::PermissionRequest` with `request_id: Option<String>` and `detail: Option<String>`. Add new variant `ApprovalDecision { request_id, approved }`. |
| `src/notify.rs` | In `parse_claude_code`: generate a `request_id` and populate `detail` on `permission-request` arm. In `handle_notify`: special-case `permission-request` to do the blocking round-trip and emit Claude's decision JSON on stdout. |
| `src/main.rs` | `mod approval;`. Thread `ApprovalRegistry` + `show_sender` closure through the daemon. Rewrite `PermissionRequest` arm in `handle_connection` to stash the `OwnedWriteHalf` in the registry and return; add `ApprovalDecision` arm that takes the entry, writes response, clears session state. Spawn a background reaper task. |
| `src/panel/session_row.rs` | When `session.pending_approval.is_some()`, append a horizontal box with Accept + Deny buttons below the action line; wire click handlers to send `ApprovalDecision` via the IPC socket. |
| `assets/style.css` | New `.approval-bar`, `.approval-accept`, `.approval-deny` rules. |
| `~/.claude/settings.json` | Remove `"async": true` from the `PermissionRequest` hook entry so Claude Code waits for the hook to exit. |

---

## Shared Types & Signatures

These names appear across multiple tasks — keep them consistent:

```rust
// src/session.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApproval {
    pub request_id: String,
    pub tool: String,
    pub detail: Option<String>,
}

pub struct Session {
    // ...existing fields...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_approval: Option<PendingApproval>,
}

// src/ipc.rs
pub enum InboundEvent {
    // ...existing variants...
    PermissionRequest {
        session_id: String,
        #[serde(default)]
        request_id: Option<String>,
        #[serde(default)]
        tool: Option<String>,
        #[serde(default)]
        detail: Option<String>,
        #[serde(default)]
        pid: Option<u32>,
    },
    ApprovalDecision {
        request_id: String,
        approved: bool,
    },
    // ...
}

// src/approval.rs
use tokio::net::unix::OwnedWriteHalf;

pub struct ApprovalEntry {
    pub write_half: OwnedWriteHalf,
    pub session_id: String,
    pub created_at: std::time::Instant,
}

pub struct ApprovalRegistry {
    inner: std::sync::Arc<tokio::sync::Mutex<
        std::collections::HashMap<String, ApprovalEntry>
    >>,
}

impl ApprovalRegistry {
    pub fn new() -> Self;
    pub fn clone(&self) -> Self;            // cheap Arc clone
    pub async fn insert(&self, request_id: String, entry: ApprovalEntry);
    pub async fn take(&self, request_id: &str) -> Option<ApprovalEntry>;
    pub async fn reap_stale(&self, max_age: std::time::Duration) -> Vec<ApprovalEntry>;
}
```

`request_id` format: `format!("{}-{}-{}", session_id, pid, nanos)` where `nanos` is `SystemTime::now().duration_since(UNIX_EPOCH).as_nanos()`. Human-readable, unique, no new dep. Used only as an opaque key — do not parse.

Decision wire format (daemon → hook): one line of JSON `{"approved":true}\n` or `{"approved":false}\n`.
Claude Code hook output (hook → stdout): `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","permissionDecision":"allow","permissionDecisionReason":"via vibewatch widget"}}` (replace `allow` with `deny` or `ask` as appropriate).

---

## Task 1: Add `PendingApproval` type and field to `Session`

**Files:**
- Modify: `src/session.rs` (struct around lines 70-99, `Session::new` around lines 101-126, tests module around line 381)

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `src/session.rs`:

```rust
#[test]
fn new_session_has_no_pending_approval() {
    let s = Session::new("s1".into(), AgentKind::ClaudeCode, 42);
    assert!(s.pending_approval.is_none());
}

#[test]
fn session_serializes_pending_approval_when_set() {
    let mut s = Session::new("s1".into(), AgentKind::ClaudeCode, 42);
    s.pending_approval = Some(PendingApproval {
        request_id: "req-xyz".into(),
        tool: "Bash".into(),
        detail: Some("rm -rf /tmp/foo".into()),
    });
    let json = serde_json::to_string(&s).unwrap();
    assert!(json.contains(r#""pending_approval":"#));
    assert!(json.contains(r#""request_id":"req-xyz""#));
    assert!(json.contains(r#""tool":"Bash""#));
    assert!(json.contains(r#""detail":"rm -rf /tmp/foo""#));
}

#[test]
fn session_omits_pending_approval_when_none() {
    let s = Session::new("s1".into(), AgentKind::ClaudeCode, 42);
    let json = serde_json::to_string(&s).unwrap();
    assert!(!json.contains("pending_approval"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo test --lib session::tests::new_session_has_no_pending_approval -- --nocapture
```

Expected: FAIL with "no field `pending_approval`" and "no struct `PendingApproval`".

- [ ] **Step 3: Add the `PendingApproval` struct**

Insert above `pub struct Session` (around line 69 in `src/session.rs`):

```rust
/// A pending tool-approval request from the agent, awaiting the user's
/// widget click. Serializable so it appears in `vibewatch status` output;
/// the held socket stream lives in `ApprovalRegistry`, not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApproval {
    pub request_id: String,
    pub tool: String,
    #[serde(default)]
    pub detail: Option<String>,
}
```

- [ ] **Step 4: Add the field to `Session`**

Inside `pub struct Session { ... }`, just before the closing brace, add:

```rust
    /// Set while the session is waiting on a user Accept/Deny click in
    /// the widget. `None` at all other times.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_approval: Option<PendingApproval>,
```

- [ ] **Step 5: Initialize in `Session::new`**

Inside `Session::new`, add the new field to the struct literal (just after `transcript_path: None,`):

```rust
            pending_approval: None,
```

- [ ] **Step 6: Run tests to verify they pass**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo test --lib session::tests -- --nocapture
```

Expected: all session tests PASS, including the three new ones.

- [ ] **Step 7: Commit**

```bash
cd /home/moinax/Projects/labs/vibewatch
git add src/session.rs
git commit -m "session: add PendingApproval struct and session.pending_approval field"
```

---

## Task 2: Extend IPC events with `request_id` / `detail` / `ApprovalDecision`

**Files:**
- Modify: `src/ipc.rs` (enum around lines 7-61, tests module around line 167)

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `src/ipc.rs`:

```rust
    #[test]
    fn test_parse_permission_request_with_new_fields() {
        let json = r#"{"event":"permission_request","session_id":"s1","request_id":"r42","tool":"Bash","detail":"ls -la","pid":123}"#;
        let event: InboundEvent = serde_json::from_str(json).unwrap();
        match event {
            InboundEvent::PermissionRequest {
                session_id,
                request_id,
                tool,
                detail,
                pid,
            } => {
                assert_eq!(session_id, "s1");
                assert_eq!(request_id.as_deref(), Some("r42"));
                assert_eq!(tool.as_deref(), Some("Bash"));
                assert_eq!(detail.as_deref(), Some("ls -la"));
                assert_eq!(pid, Some(123));
            }
            _ => panic!("expected PermissionRequest"),
        }
    }

    #[test]
    fn test_parse_permission_request_without_optional_fields_still_works() {
        // Backwards-compat: older async-style hook may only set session_id + tool.
        let json = r#"{"event":"permission_request","session_id":"s1","tool":"Bash"}"#;
        let event: InboundEvent = serde_json::from_str(json).unwrap();
        match event {
            InboundEvent::PermissionRequest {
                session_id,
                request_id,
                detail,
                pid,
                ..
            } => {
                assert_eq!(session_id, "s1");
                assert!(request_id.is_none());
                assert!(detail.is_none());
                assert!(pid.is_none());
            }
            _ => panic!("expected PermissionRequest"),
        }
    }

    #[test]
    fn test_parse_approval_decision() {
        let json = r#"{"event":"approval_decision","request_id":"r42","approved":true}"#;
        let event: InboundEvent = serde_json::from_str(json).unwrap();
        match event {
            InboundEvent::ApprovalDecision { request_id, approved } => {
                assert_eq!(request_id, "r42");
                assert!(approved);
            }
            _ => panic!("expected ApprovalDecision"),
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo test --lib ipc::tests -- --nocapture
```

Expected: FAIL on compile with "no field `request_id`" / "no variant `ApprovalDecision`".

- [ ] **Step 3: Extend `PermissionRequest` and add `ApprovalDecision`**

In `src/ipc.rs`, replace the existing `PermissionRequest` variant (around lines 42-48) with:

```rust
    PermissionRequest {
        session_id: String,
        #[serde(default)]
        request_id: Option<String>,
        #[serde(default)]
        tool: Option<String>,
        #[serde(default)]
        detail: Option<String>,
        #[serde(default)]
        pid: Option<u32>,
    },
```

Add a new variant just before the closing `}` of the enum (after the existing `TogglePanel`):

```rust
    ApprovalDecision {
        request_id: String,
        approved: bool,
    },
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo test --lib ipc::tests -- --nocapture
```

Expected: all IPC tests PASS.

- [ ] **Step 5: Fix main.rs call sites that break**

The `PermissionRequest` arm in `src/main.rs` (around line 297) no longer compiles — it destructures only three fields. Update it to:

```rust
            InboundEvent::PermissionRequest {
                session_id,
                request_id: _,
                tool,
                detail: _,
                pid,
            } => {
                if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
                    session.status = SessionStatus::WaitingApproval;
                    session.current_tool = tool;
                    session.touch();
                    registry.register(session);
                    sound_player.play(SoundEvent::ApprovalNeeded);
                }
            }
```

This is a temporary shape — Task 7 will rewrite this arm with the full blocking logic. For now we just keep main.rs compiling.

- [ ] **Step 6: Add a stub `ApprovalDecision` arm so the match remains exhaustive**

In the same `match event { ... }` block in `handle_connection`, append another arm before the closing `}`:

```rust
            InboundEvent::ApprovalDecision { .. } => {
                // Handled in Task 7.
            }
```

- [ ] **Step 7: Full build to verify**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo build
```

Expected: clean build (warnings about unused fields are fine).

- [ ] **Step 8: Commit**

```bash
cd /home/moinax/Projects/labs/vibewatch
git add src/ipc.rs src/main.rs
git commit -m "ipc: add request_id/detail to PermissionRequest; add ApprovalDecision variant"
```

---

## Task 3: Build the `ApprovalRegistry` module

**Files:**
- Create: `src/approval.rs`
- Modify: `src/main.rs` (add `mod approval;` near the top, right after the existing `mod` lines)

- [ ] **Step 1: Write the failing tests**

Create `src/approval.rs` with ONLY the test module (no types yet):

```rust
//! `ApprovalRegistry` — holds open hook connections keyed by `request_id` so
//! the daemon can write a decision back when the user clicks Accept/Deny in
//! the widget.

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::net::{UnixListener, UnixStream};

    /// Spawn a throwaway UnixStream pair and return the "server side" which
    /// we'll split into halves — the write half is what gets stashed.
    async fn make_pair() -> UnixStream {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let client = UnixStream::connect(&path).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        drop(client);
        drop(listener);
        drop(tmp);
        server
    }

    #[tokio::test]
    async fn insert_and_take_roundtrip() {
        let reg = ApprovalRegistry::new();
        let stream = make_pair().await;
        let (_rh, wh) = stream.into_split();
        let entry = ApprovalEntry {
            write_half: wh,
            session_id: "s1".into(),
            created_at: std::time::Instant::now(),
        };
        reg.insert("req-1".into(), entry).await;

        let taken = reg.take("req-1").await.expect("should find entry");
        assert_eq!(taken.session_id, "s1");
        assert!(reg.take("req-1").await.is_none(), "second take returns None");
    }

    #[tokio::test]
    async fn take_unknown_returns_none() {
        let reg = ApprovalRegistry::new();
        assert!(reg.take("does-not-exist").await.is_none());
    }

    #[tokio::test]
    async fn reap_stale_removes_old_entries() {
        let reg = ApprovalRegistry::new();
        let stream = make_pair().await;
        let (_rh, wh) = stream.into_split();
        let old_entry = ApprovalEntry {
            write_half: wh,
            session_id: "old".into(),
            created_at: std::time::Instant::now() - Duration::from_secs(700),
        };
        reg.insert("old-req".into(), old_entry).await;

        let stream2 = make_pair().await;
        let (_rh2, wh2) = stream2.into_split();
        let fresh_entry = ApprovalEntry {
            write_half: wh2,
            session_id: "fresh".into(),
            created_at: std::time::Instant::now(),
        };
        reg.insert("fresh-req".into(), fresh_entry).await;

        let reaped = reg.reap_stale(Duration::from_secs(580)).await;
        assert_eq!(reaped.len(), 1);
        assert_eq!(reaped[0].session_id, "old");
        assert!(reg.take("old-req").await.is_none());
        assert!(reg.take("fresh-req").await.is_some());
    }

    #[tokio::test]
    async fn clone_shares_storage() {
        let reg = ApprovalRegistry::new();
        let reg2 = reg.clone();
        let stream = make_pair().await;
        let (_rh, wh) = stream.into_split();
        reg.insert(
            "r1".into(),
            ApprovalEntry {
                write_half: wh,
                session_id: "s1".into(),
                created_at: std::time::Instant::now(),
            },
        )
        .await;
        assert!(reg2.take("r1").await.is_some(), "clone sees the entry");
    }
}
```

- [ ] **Step 2: Wire the module into the binary**

In `src/main.rs`, add `mod approval;` in the `mod` block near the top (alphabetical with the others — between `mod notify;` and `mod scanner;`):

```rust
mod compositor;
mod config;
mod ipc;
mod notify;
mod approval;
mod scanner;
mod session;
mod transcript;
mod sound;
mod waybar;
```

- [ ] **Step 3: Run tests to verify they fail**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo test --lib approval -- --nocapture
```

Expected: FAIL with "cannot find type `ApprovalRegistry`" / "cannot find type `ApprovalEntry`".

- [ ] **Step 4: Implement the types**

Prepend the types above the `#[cfg(test)] mod tests` block in `src/approval.rs`:

```rust
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::Mutex;

/// One held hook connection awaiting the user's Accept/Deny click.
pub struct ApprovalEntry {
    pub write_half: OwnedWriteHalf,
    pub session_id: String,
    pub created_at: Instant,
}

/// Thread-safe map of `request_id` → pending approval. Cheaply cloneable.
#[derive(Clone)]
pub struct ApprovalRegistry {
    inner: Arc<Mutex<HashMap<String, ApprovalEntry>>>,
}

impl ApprovalRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn insert(&self, request_id: String, entry: ApprovalEntry) {
        let mut map = self.inner.lock().await;
        map.insert(request_id, entry);
    }

    pub async fn take(&self, request_id: &str) -> Option<ApprovalEntry> {
        let mut map = self.inner.lock().await;
        map.remove(request_id)
    }

    /// Remove and return any entries older than `max_age`.
    pub async fn reap_stale(&self, max_age: Duration) -> Vec<ApprovalEntry> {
        let mut map = self.inner.lock().await;
        let now = Instant::now();
        let stale: Vec<String> = map
            .iter()
            .filter(|(_, e)| now.duration_since(e.created_at) > max_age)
            .map(|(k, _)| k.clone())
            .collect();
        stale.into_iter().filter_map(|k| map.remove(&k)).collect()
    }
}

impl Default for ApprovalRegistry {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo test --lib approval -- --nocapture
```

Expected: all four approval tests PASS.

- [ ] **Step 6: Commit**

```bash
cd /home/moinax/Projects/labs/vibewatch
git add src/approval.rs src/main.rs
git commit -m "approval: add ApprovalRegistry holding pending hook writers by request_id"
```

---

## Task 4: Populate `request_id` and `detail` in the hook's `PermissionRequest` event

**Files:**
- Modify: `src/notify.rs` (function `parse_claude_code`, around line 123, and helper `parent_pid` already exists)

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `src/notify.rs`:

```rust
    #[test]
    fn test_parse_claude_code_permission_request_sets_all_fields() {
        let json = r#"{"session_id":"abc123","hook_event_name":"permission-request","tool_name":"Bash","tool_input":{"command":"rm -rf /tmp"}}"#;
        let event = parse_claude_code(json, "permission-request").unwrap();
        match event {
            InboundEvent::PermissionRequest {
                session_id,
                request_id,
                tool,
                detail,
                pid,
            } => {
                assert_eq!(session_id, "abc123");
                let rid = request_id.expect("request_id must be set by hook");
                assert!(rid.contains("abc123"), "request_id should contain session_id, got {:?}", rid);
                assert_eq!(tool.as_deref(), Some("Bash"));
                assert_eq!(detail.as_deref(), Some("rm -rf /tmp"));
                assert!(pid.is_some());
            }
            _ => panic!("expected PermissionRequest"),
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo test --lib notify::tests::test_parse_claude_code_permission_request_sets_all_fields -- --nocapture
```

Expected: FAIL — current code doesn't emit `request_id` or `detail`.

- [ ] **Step 3: Rewrite the `permission-request` arm**

In `src/notify.rs`, find the `"permission-request" =>` arm inside `parse_claude_code` (around line 123). Replace it with:

```rust
        "permission-request" => {
            let pid = parent_pid();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let request_id = format!("{}-{}-{}", hook.session_id, pid, nanos);
            Ok(InboundEvent::PermissionRequest {
                session_id: hook.session_id,
                request_id: Some(request_id),
                tool: hook.tool_name,
                detail: extract_tool_detail(&hook.tool_input),
                pid: Some(pid),
            })
        }
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo test --lib notify::tests -- --nocapture
```

Expected: all notify tests PASS (new one included).

- [ ] **Step 5: Commit**

```bash
cd /home/moinax/Projects/labs/vibewatch
git add src/notify.rs
git commit -m "notify: populate request_id and detail on permission-request hook"
```

---

## Task 5: Make the hook block for a decision and emit Claude's JSON

**Files:**
- Modify: `src/notify.rs` (`handle_notify`, around line 40; add a new `send_permission_request` helper)
- Modify: `src/ipc.rs` (expose the low-level `write_json` variant that keeps the connection open — may already be sufficient; see step 3)

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `src/notify.rs`:

```rust
    #[tokio::test]
    async fn send_permission_request_reads_decision_line() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        use tokio::net::{UnixListener, UnixStream};

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("v.sock");
        let listener = UnixListener::bind(&path).unwrap();

        let server_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = stream.split();
            let mut reader = tokio::io::BufReader::new(read_half);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            assert!(line.contains("\"event\":\"permission_request\""));
            write_half
                .write_all(b"{\"approved\":true}\n")
                .await
                .unwrap();
            write_half.flush().await.unwrap();
            // Hold the stream until the client drops
            let mut discard = String::new();
            let _ = reader.read_line(&mut discard).await;
        });

        let event = InboundEvent::PermissionRequest {
            session_id: "s1".into(),
            request_id: Some("r1".into()),
            tool: Some("Bash".into()),
            detail: Some("ls".into()),
            pid: Some(42),
        };
        let decision = send_permission_request(&path, &event, std::time::Duration::from_secs(2))
            .await
            .expect("round-trip succeeds");
        assert_eq!(decision, PermissionDecision::Allow);

        let _ = server_task.await;
    }

    #[tokio::test]
    async fn send_permission_request_errors_when_daemon_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("v.sock");
        // Don't bind a listener — connect will fail, function returns Err.
        let event = InboundEvent::PermissionRequest {
            session_id: "s1".into(),
            request_id: Some("r1".into()),
            tool: Some("Bash".into()),
            detail: None,
            pid: None,
        };
        let result = send_permission_request(&path, &event, std::time::Duration::from_millis(100)).await;
        assert!(result.is_err(), "missing daemon socket should produce an error");
    }
```

Note: the higher-level `handle_notify` translates this `Err` into `PermissionDecision::Ask` so Claude's terminal prompt takes over (see Step 5 below).

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo test --lib notify::tests::send_permission_request_reads_decision_line -- --nocapture
```

Expected: FAIL — `send_permission_request` and `PermissionDecision` do not exist.

- [ ] **Step 3: Add the helper + enum**

At the top of `src/notify.rs`, just after the existing `use` block, add:

```rust
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Outcome of a blocking permission-request round-trip with the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny,
    Ask,
}

impl PermissionDecision {
    fn as_claude_str(self) -> &'static str {
        match self {
            PermissionDecision::Allow => "allow",
            PermissionDecision::Deny => "deny",
            PermissionDecision::Ask => "ask",
        }
    }
}

/// Connect to the daemon, send a `PermissionRequest` event, keep the stream
/// open and block reading one JSON decision line. `timeout` bounds the whole
/// exchange. Returns the parsed decision. Connection failure is returned as
/// `Err` so the caller can translate to `Ask`.
pub async fn send_permission_request(
    socket_path: &std::path::Path,
    event: &InboundEvent,
    timeout: std::time::Duration,
) -> anyhow::Result<PermissionDecision> {
    use anyhow::Context;
    let mut stream = UnixStream::connect(socket_path)
        .await
        .context("connect to vibewatch daemon")?;

    let mut json = serde_json::to_string(event)?;
    json.push('\n');
    stream.write_all(json.as_bytes()).await?;
    stream.flush().await?;

    let (read_half, _write_half) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(read_half);
    let mut line = String::new();

    let read_fut = reader.read_line(&mut line);
    match tokio::time::timeout(timeout, read_fut).await {
        Ok(Ok(n)) if n > 0 => {
            let v: serde_json::Value = serde_json::from_str(line.trim())?;
            let approved = v.get("approved").and_then(|x| x.as_bool()).unwrap_or(false);
            Ok(if approved {
                PermissionDecision::Allow
            } else {
                PermissionDecision::Deny
            })
        }
        Ok(Ok(_)) => Ok(PermissionDecision::Ask), // EOF — treat as fallback
        Ok(Err(e)) => Err(anyhow::anyhow!("read error: {e}")),
        Err(_) => Ok(PermissionDecision::Ask),    // timeout
    }
}
```

- [ ] **Step 4: Run helper tests to verify they pass**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo test --lib notify::tests::send_permission_request_reads_decision_line -- --nocapture
cargo test --lib notify::tests::send_permission_request_times_out_as_ask -- --nocapture
```

Expected: both PASS.

- [ ] **Step 5: Route `permission-request` through the blocking helper in `handle_notify`**

In `src/notify.rs`, find `handle_notify` (around line 40). Replace its body so Claude-Code `permission-request` uses the new helper and writes Claude's JSON to stdout. The final function should look like:

```rust
pub async fn handle_notify(event_type: &str, agent: &str) -> anyhow::Result<()> {
    let mut stdin_buf = String::new();
    std::io::stdin()
        .read_to_string(&mut stdin_buf)
        .context("failed to read stdin")?;

    let event = match agent {
        "claude-code" => parse_claude_code(&stdin_buf, event_type)?,
        "codex" => parse_codex(&stdin_buf, event_type)?,
        other => bail!("unknown agent: {}", other),
    };

    let config = Config::load()?;
    let socket_path = config.socket_path();

    if agent == "claude-code" && event_type == "permission-request" {
        let decision = match send_permission_request(
            &socket_path,
            &event,
            std::time::Duration::from_secs(580),
        )
        .await
        {
            Ok(d) => d,
            Err(e) => {
                eprintln!("vibewatch: permission-request fallback ask ({e})");
                PermissionDecision::Ask
            }
        };
        let out = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "permissionDecision": decision.as_claude_str(),
                "permissionDecisionReason": "via vibewatch widget",
            }
        });
        println!("{}", serde_json::to_string(&out)?);
        return Ok(());
    }

    send_event(&socket_path, &event).await?;
    Ok(())
}
```

- [ ] **Step 6: Verify full build + tests**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo build
cargo test --lib notify -- --nocapture
```

Expected: clean build; all notify tests PASS.

- [ ] **Step 7: Commit**

```bash
cd /home/moinax/Projects/labs/vibewatch
git add src/notify.rs
git commit -m "notify: block permission-request hook on daemon socket, emit Claude decision JSON"
```

---

## Task 6: Add a `show_sender` closure in the GTK daemon wiring

**Files:**
- Modify: `src/main.rs` (inside `run_daemon_with_panel`, around lines 130-145)

Rationale: the daemon needs a way to call `window.set_visible(true)` from the tokio side when a `PermissionRequest` arrives. Mirror the existing `toggle_fn` pattern.

- [ ] **Step 1: Add the `show_fn` closure alongside `toggle_fn`**

In `src/main.rs` inside `app.connect_activate(move |app| { ... })` (around lines 130-145), insert the following just after `toggle_fn` is created:

```rust
        let show_weak = glib::SendWeakRef::from(window.downgrade());
        let show_fn: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let show_weak = show_weak.clone();
            glib::MainContext::default().invoke(move || {
                if let Some(win) = show_weak.upgrade() {
                    win.set_visible(true);
                    win.present();
                }
            });
        });
```

- [ ] **Step 2: Thread `show_fn` into `handle_connection`**

Change the `handle_connection` signature in `src/main.rs` to accept it (add parameter):

```rust
async fn handle_connection(
    stream: tokio::net::UnixStream,
    registry: SessionRegistry,
    sound_player: Arc<SoundPlayer>,
    toggle_sender: Option<Arc<dyn Fn() + Send + Sync>>,
    show_sender: Option<Arc<dyn Fn() + Send + Sync>>,
    approval_registry: crate::approval::ApprovalRegistry,
) {
```

Update both callers:

In `run_daemon_headless` (around line 105-116), change the `tokio::spawn` block:

```rust
        let approval_registry = approval_registry.clone();
        tokio::spawn(async move {
            handle_connection(
                stream,
                registry,
                sound_player,
                None::<Arc<dyn Fn() + Send + Sync>>,
                None::<Arc<dyn Fn() + Send + Sync>>,
                approval_registry,
            )
            .await;
        });
```

…and declare `let approval_registry = crate::approval::ApprovalRegistry::new();` just before the loop in `run_daemon_headless`.

In `run_daemon_with_panel` (around line 148-196), the edits live **inside** `rt.block_on(async move { ... })` on the spawned thread:

1. Just after the scanner `tokio::spawn(...)` block and **before** the `loop { match server.accept()... }` loop, add:

   ```rust
                   let approval_registry = crate::approval::ApprovalRegistry::new();
   ```

2. Inside the `Ok(stream) =>` arm of the accept match, before the `tokio::spawn(async move { ... })`, update to:

   ```rust
                           let registry = registry.clone();
                           let sound_player = sound_player.clone();
                           let toggle_fn = toggle_fn.clone();
                           let show_fn = show_fn.clone();
                           let approval_registry = approval_registry.clone();
                           tokio::spawn(async move {
                               handle_connection(
                                   stream,
                                   registry,
                                   sound_player,
                                   Some(toggle_fn),
                                   Some(show_fn),
                                   approval_registry,
                               )
                               .await;
                           });
   ```

Take care: the `approval_registry.clone()` and `show_fn.clone()` must happen **inside** the `match server.accept()` arm so each spawned handler owns its own clone.

- [ ] **Step 3: Verify it still compiles**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo build
```

Expected: clean build. The new parameters are unused (warning is OK) — Task 7 wires them up.

- [ ] **Step 4: Commit**

```bash
cd /home/moinax/Projects/labs/vibewatch
git add src/main.rs
git commit -m "main: thread ApprovalRegistry and show_fn closure into handle_connection"
```

---

## Task 7: Wire `PermissionRequest` and `ApprovalDecision` in the daemon

**Files:**
- Modify: `src/main.rs` (`handle_connection`, the `PermissionRequest` and `ApprovalDecision` match arms around lines 297-318)

- [ ] **Step 1: Refactor `handle_connection` to keep `write_half` owned**

The existing function already takes `stream.into_split()` at the top (line 214). The `PermissionRequest` branch needs to **move** the `write_half` into the registry and `return`. Current code uses `write_half` only for `GetStatus`. Restructure so that `write_half` is owned by the outer loop and can be moved out on the permission branch.

Replace the entire body of `handle_connection` with:

```rust
async fn handle_connection(
    stream: tokio::net::UnixStream,
    registry: SessionRegistry,
    sound_player: Arc<SoundPlayer>,
    toggle_sender: Option<Arc<dyn Fn() + Send + Sync>>,
    show_sender: Option<Arc<dyn Fn() + Send + Sync>>,
    approval_registry: crate::approval::ApprovalRegistry,
) {
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
                registry.remove_by_pid(pid);
                let kind = parse_agent_kind(&agent);
                let mut session = Session::new(session_id, kind, pid);
                session.cwd = cwd;
                session.session_name = session_name;
                session.terminal = Some(session::detect_terminal(pid));
                registry.register(session);
            }
            InboundEvent::PreToolUse {
                session_id,
                tool,
                detail,
                pid,
            } => {
                if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
                    session.status = SessionStatus::Executing;
                    session.current_tool = Some(tool);
                    session.tool_detail = detail;
                    session.touch();
                    registry.register(session);
                }
            }
            InboundEvent::PostToolUse {
                session_id,
                tool: _,
                success,
                pid,
            } => {
                if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
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
            InboundEvent::UserPromptSubmit {
                session_id,
                prompt,
                pid,
            } => {
                if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
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
            InboundEvent::PermissionRequest {
                session_id,
                request_id,
                tool,
                detail,
                pid,
            } => {
                let request_id = match request_id {
                    Some(r) => r,
                    None => {
                        // Old fire-and-forget caller: just flip status and continue.
                        if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
                            session.status = SessionStatus::WaitingApproval;
                            session.current_tool = tool;
                            session.touch();
                            registry.register(session);
                            sound_player.play(SoundEvent::ApprovalNeeded);
                        }
                        continue;
                    }
                };
                let tool_name = tool.clone().unwrap_or_else(|| "tool".into());

                if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
                    session.status = SessionStatus::WaitingApproval;
                    session.current_tool = Some(tool_name.clone());
                    session.tool_detail = detail.clone();
                    session.pending_approval = Some(crate::session::PendingApproval {
                        request_id: request_id.clone(),
                        tool: tool_name,
                        detail,
                    });
                    session.touch();
                    registry.register(session);
                }
                sound_player.play(SoundEvent::ApprovalNeeded);
                if let Some(ref show) = show_sender {
                    show();
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
                }
            }
            InboundEvent::ApprovalDecision { request_id, approved } => {
                if let Some(mut entry) = approval_registry.take(&request_id).await {
                    let line = if approved {
                        b"{\"approved\":true}\n".as_slice()
                    } else {
                        b"{\"approved\":false}\n".as_slice()
                    };
                    let _ = entry.write_half.write_all(line).await;
                    let _ = entry.write_half.flush().await;
                    // Clear the session's pending approval.
                    if let Some(mut s) = registry.get(&entry.session_id) {
                        s.pending_approval = None;
                        s.status = SessionStatus::Thinking;
                        s.current_tool = None;
                        s.tool_detail = None;
                        s.touch();
                        registry.register(s);
                    }
                }
            }
            InboundEvent::Stop { session_id, pid } => {
                if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
                    session.status = SessionStatus::Idle;
                    session.current_tool = None;
                    session.tool_detail = None;
                    session.pending_approval = None;
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
                let registry = registry.clone();
                let sid = session_id.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                    if let Some(mut session) = registry.get(&sid) {
                        let agent = session.agent;
                        if let Some(text) = transcript::read_last_assistant_line(
                            agent,
                            &sid,
                            &mut session.transcript_path,
                        ) {
                            session.last_agent_text = Some(text);
                            session.last_agent_text_at = now_epoch();
                            registry.register(session);
                        }
                    }
                });
            }
            InboundEvent::GetStatus => {
                let sessions = registry.all();
                let status = waybar::build_status(&sessions);
                let mut json = serde_json::to_string(&status).unwrap_or_default();
                json.push('\n');
                let _ = write_half.write_all(json.as_bytes()).await;
                let _ = write_half.flush().await;
                return;
            }
            InboundEvent::TogglePanel => {
                if let Some(ref sender) = toggle_sender {
                    sender();
                }
            }
        }
    }
}
```

Note that the `Stop` arm now also clears `pending_approval` (safety) and its delayed-reread `tokio::spawn` owns `sid` cleanly.

- [ ] **Step 2: Full build**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo build
```

Expected: clean build.

- [ ] **Step 3: Smoke-test with a manual round-trip**

Run all existing tests to make sure we didn't break anything:

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo test --lib -- --nocapture
```

Expected: all tests PASS.

- [ ] **Step 4: Commit**

```bash
cd /home/moinax/Projects/labs/vibewatch
git add src/main.rs
git commit -m "main: daemon handles blocking PermissionRequest + ApprovalDecision, auto-shows panel"
```

---

## Task 8: Background reaper for stale approval entries

**Files:**
- Modify: `src/main.rs` (inside `run_daemon_with_panel` after `let scanner_registry`, around line 175, and symmetric change in `run_daemon_headless`)

- [ ] **Step 1: Spawn the reaper in `run_daemon_with_panel`**

Inside `run_daemon_with_panel`, after the scanner `tokio::spawn` block and before the `loop { match server.accept()... }` loop, insert:

```rust
                let reaper_registry = registry.clone();
                let reaper_approval = approval_registry.clone();
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
                            }
                            // Dropping `entry` closes the write half so the hook read returns EOF.
                        }
                    }
                });
```

- [ ] **Step 2: Same reaper in `run_daemon_headless`**

In `run_daemon_headless`, after `tokio::spawn(scanner::run_scanner(...))` and before the accept loop, insert the same block (with `approval_registry` and `registry` substituted for the locals in scope).

- [ ] **Step 3: Build**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo build
```

Expected: clean build.

- [ ] **Step 4: Commit**

```bash
cd /home/moinax/Projects/labs/vibewatch
git add src/main.rs
git commit -m "main: reap stale approval registry entries every 30s"
```

---

## Task 9: Render Accept/Deny buttons in the session row

**Files:**
- Modify: `src/panel/session_row.rs` (`build_row`, around lines 10-94; add a helper `build_approval_bar`)

- [ ] **Step 1: Write the failing test**

Append to the `tests` module at the bottom of `src/panel/session_row.rs`:

```rust
    #[test]
    fn has_pending_approval_returns_true_when_set() {
        let mut s = mk(AgentKind::ClaudeCode);
        s.pending_approval = Some(crate::session::PendingApproval {
            request_id: "r1".into(),
            tool: "Bash".into(),
            detail: Some("ls".into()),
        });
        assert!(has_pending_approval(&s));
    }

    #[test]
    fn has_pending_approval_returns_false_when_none() {
        let s = mk(AgentKind::ClaudeCode);
        assert!(!has_pending_approval(&s));
    }
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo test panel::session_row::tests::has_pending_approval_returns_true_when_set -- --nocapture
```

Expected: FAIL — `has_pending_approval` is not defined.

- [ ] **Step 3: Add the predicate + approval-bar builder**

In `src/panel/session_row.rs`, just above the existing `fn describe` (around line 121), add:

```rust
/// Test hook: does this session currently expect a widget approval click?
pub(crate) fn has_pending_approval(session: &Session) -> bool {
    session.pending_approval.is_some()
}

/// Build a horizontal box containing Accept + Deny buttons, wired to send
/// `ApprovalDecision` over the IPC socket when clicked.
fn build_approval_bar(request_id: String) -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.add_css_class("approval-bar");
    bar.set_halign(gtk::Align::Start);
    bar.set_margin_top(4);

    let accept = gtk::Button::with_label("Accept");
    accept.add_css_class("approval-accept");
    let rid_a = request_id.clone();
    accept.connect_clicked(move |_| {
        let rid = rid_a.clone();
        std::thread::spawn(move || {
            send_approval_decision(&rid, true);
        });
    });
    bar.append(&accept);

    let deny = gtk::Button::with_label("Deny");
    deny.add_css_class("approval-deny");
    let rid_d = request_id;
    deny.connect_clicked(move |_| {
        let rid = rid_d.clone();
        std::thread::spawn(move || {
            send_approval_decision(&rid, false);
        });
    });
    bar.append(&deny);

    bar
}
```

- [ ] **Step 4: Render the bar from `build_row`**

In `src/panel/session_row.rs` `build_row`, after the block that appends `action_label` (just before `card.append(&content);`, around line 77-78), insert:

```rust
    if let Some(ref pending) = session.pending_approval {
        let bar = build_approval_bar(pending.request_id.clone());
        content.append(&bar);
    }
```

- [ ] **Step 5: Add a stub `send_approval_decision` so the file compiles**

At the bottom of `src/panel/session_row.rs` (before `#[cfg(test)] mod tests`), add a placeholder that Task 10 fills in:

```rust
/// Stub — implemented in Task 10.
fn send_approval_decision(_request_id: &str, _approved: bool) {
    eprintln!("send_approval_decision: stub (Task 10 wires this up)");
}
```

- [ ] **Step 6: Verify tests pass**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo test panel::session_row::tests::has_pending_approval -- --nocapture
cargo build
```

Expected: tests PASS; clean build.

- [ ] **Step 7: Commit**

```bash
cd /home/moinax/Projects/labs/vibewatch
git add src/panel/session_row.rs
git commit -m "panel: render Accept/Deny buttons when pending_approval is set"
```

---

## Task 10: Wire button clicks to send `ApprovalDecision` via IPC

**Files:**
- Modify: `src/panel/session_row.rs` (replace the stub `send_approval_decision`)

- [ ] **Step 1: Replace the stub with a real implementation**

In `src/panel/session_row.rs`, replace the stub at the bottom of the file with:

```rust
fn send_approval_decision(request_id: &str, approved: bool) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("vibewatch: failed to build tokio rt for approval: {e}");
            return;
        }
    };
    let request_id = request_id.to_string();
    rt.block_on(async move {
        let config = match crate::config::Config::load() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("vibewatch: config load failed: {e}");
                return;
            }
        };
        let event = crate::ipc::InboundEvent::ApprovalDecision {
            request_id,
            approved,
        };
        if let Err(e) = crate::ipc::send_event(&config.socket_path(), &event).await {
            eprintln!("vibewatch: send_event ApprovalDecision failed: {e}");
        }
    });
}
```

- [ ] **Step 2: Verify it builds**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo build
```

Expected: clean build.

- [ ] **Step 3: Commit**

```bash
cd /home/moinax/Projects/labs/vibewatch
git add src/panel/session_row.rs
git commit -m "panel: send ApprovalDecision IPC event on Accept/Deny click"
```

---

## Task 11: Style the Accept / Deny buttons

**Files:**
- Modify: `assets/style.css` (append a new section at the bottom)

- [ ] **Step 1: Append the CSS**

Append the following at the bottom of `assets/style.css`:

```css
/* ── Approval bar (Accept / Deny) ─────────────────────────────── */

.approval-bar {
    margin-top: 6px;
}

.approval-accept,
.approval-deny {
    font-size: 11px;
    font-weight: 600;
    padding: 3px 12px;
    border-radius: 6px;
    border: 1px solid transparent;
    min-height: 22px;
}

.approval-accept {
    color: #1e1e2e;
    background-color: #a6e3a1;
    border-color: rgba(166, 227, 161, 0.6);
}

.approval-accept:hover {
    background-color: #b8ecb3;
    box-shadow: 0 0 6px rgba(166, 227, 161, 0.35);
}

.approval-deny {
    color: #1e1e2e;
    background-color: #f38ba8;
    border-color: rgba(243, 139, 168, 0.6);
}

.approval-deny:hover {
    background-color: #f7a3ba;
    box-shadow: 0 0 6px rgba(243, 139, 168, 0.35);
}
```

- [ ] **Step 2: Verify build**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo build --release
```

Expected: clean build.

- [ ] **Step 3: Commit**

```bash
cd /home/moinax/Projects/labs/vibewatch
git add assets/style.css
git commit -m "style: Catppuccin accept/deny button styles for approval bar"
```

---

## Task 12: Update Claude Code hook config to blocking

**Files:**
- Modify: `~/.claude/settings.json` (the `PermissionRequest` entry)

Context: Claude Code's current hook config has `"async": true` on the `PermissionRequest` hook, which makes Claude not wait for the hook. The new flow needs the hook to block, so remove the flag (or set it to `false`).

- [ ] **Step 1: Edit the settings file**

Open `~/.claude/settings.json` and locate the `PermissionRequest` hook block. Change:

```json
"PermissionRequest": [
  {
    "matcher": "",
    "hooks": [
      {
        "type": "command",
        "command": "~/.cargo/bin/vibewatch notify permission-request --agent claude-code",
        "async": true
      }
    ]
  }
]
```

To:

```json
"PermissionRequest": [
  {
    "matcher": "",
    "hooks": [
      {
        "type": "command",
        "command": "~/.cargo/bin/vibewatch notify permission-request --agent claude-code"
      }
    ]
  }
]
```

(The `"async": true` line is removed entirely.)

- [ ] **Step 2: Verify JSON is still valid**

```bash
python3 -c "import json; json.load(open('/home/moinax/.claude/settings.json'))" && echo OK
```

Expected: `OK`.

No commit needed — this file isn't in the vibewatch repo. Note the change in the plan execution log if using subagent-driven-development.

---

## Task 13: Deploy and smoke-test

**Files:** none — deployment commands only.

- [ ] **Step 1: Install the new binary**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo install --path . --force
```

Expected: install succeeds and `~/.cargo/bin/vibewatch` is replaced.

- [ ] **Step 2: Restart the daemon**

```bash
pkill -f "vibewatch daemon" || true
sleep 1
nohup ~/.cargo/bin/vibewatch daemon >/tmp/vibewatch.log 2>&1 &
disown
sleep 1
pgrep -af "vibewatch daemon" | grep -v pgrep
```

Expected: a single running `vibewatch daemon` process is printed.

- [ ] **Step 3: Smoke test via Claude**

In a Claude Code terminal, ask Claude to run a command that requires approval (e.g., `rm /tmp/does-not-matter-vibewatch-test` — anything outside the permissions allow-list). Observe:
1. The vibewatch panel auto-shows.
2. The target session row displays `Bash: rm /tmp/...` followed by **Accept** (green) and **Deny** (red) buttons.
3. Click **Accept**: Claude proceeds with the command.
4. Repeat with **Deny**: Claude reports the tool call was denied.
5. Trigger another approval, then do not click. After ~580 s, observe Claude's terminal prompt appear as fallback.

- [ ] **Step 4: Inspect live state**

```bash
~/.cargo/bin/vibewatch status | python3 -m json.tool
```

While an approval is pending, the corresponding session should show `pending_approval` with `request_id`, `tool`, and `detail`. After click, `pending_approval` should be gone.

- [ ] **Step 5: Commit the final state**

No code change here; the previous commits in Tasks 1-11 constitute the feature. If any follow-up fixes were needed from smoke-testing, commit them with a message like `fix: <issue>`.

---

## Self-review checklist (run after implementation)

1. **Spec coverage**
   - §"User-facing behaviour" items 1-6: Tasks 1, 7, 9, 10, 11 (UI + auto-show), Task 12 (timeout fallback via hook). Codex unchanged — no task needed.
   - §"Mechanism" / "End-to-end flow" 1-8: Tasks 4, 5, 6, 7, 8, 12.
   - §"Data model changes" (Session, IPC, ApprovalRegistry): Tasks 1, 2, 3.
   - §"UI changes" (session_row, styling): Tasks 9, 10, 11.
   - §"Auto-show": Task 6 wiring, Task 7 call.
   - §"Timeout + fallback": Task 5 hook timeout, Task 8 daemon reaper.
   - §"Failure modes": covered — daemon-down → hook returns `ask` (Task 5); compositor refuses → panel stays hidden but hook still times out (Tasks 5, 6); user kills hook → reaper cleans up (Task 8); stale `request_id` → daemon ignores in `ApprovalDecision` arm (Task 7).

2. **Placeholder scan**: No TBD/TODO markers. All steps include code.

3. **Type consistency**
   - `PendingApproval` has `request_id: String`, `tool: String`, `detail: Option<String>` — consistent across Tasks 1, 2, 7, 9.
   - `ApprovalDecision` variant has `request_id: String`, `approved: bool` — consistent across Tasks 2, 7, 10.
   - `send_permission_request` signature is fixed in Task 5 and not redefined.
   - `PermissionDecision` enum has three variants (`Allow`, `Deny`, `Ask`) everywhere.
   - `ApprovalEntry` fields match across Task 3 (definition) and Task 7 (use).
