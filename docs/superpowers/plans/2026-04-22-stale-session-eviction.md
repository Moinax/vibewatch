# Stale session eviction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Evict ghost sessions left behind by PID reuse so waybar and the panel stop counting Claude Code terminals the user has already closed.

**Architecture:** Replace `SessionRegistry::cleanup_dead`'s path-existence probe with an agent-aware `/proc/<pid>/comm` match. The comm-name constants already used by the scanner for discovery become the single source of truth for liveness, too. Window-based sessions continue to be reaped by the compositor scan, so `cleanup_dead` skips them.

**Tech Stack:** Rust, tokio, standard library `std::fs` (for `/proc` reads). No new dependencies.

Spec: `docs/superpowers/specs/2026-04-22-stale-session-eviction-design.md`

---

## File Structure

- Modify: `src/session.rs` — add agent-comm constants, add `expected_comms_for`, add `is_agent_pid_alive` (+ a pure, testable helper), rewrite `cleanup_dead`, delete `is_pid_alive` and its test.
- Modify: `src/scanner.rs` — drop the local `CLAUDE_CODE_NAMES` / `CODEX_NAMES` constants and import them from `session.rs` instead.

No other files change. No new modules, no config, no new dependencies.

---

### Task 1: Move comm-name constants from `scanner.rs` to `session.rs`

Prep refactor. Making the constants a public part of `session.rs` gives both scanner discovery and registry eviction a single source of truth, which is what subsequent tasks depend on.

**Files:**
- Modify: `src/session.rs` (add constants near the other agent-related items, below `TOOL_ASK_USER_QUESTION` around line 10)
- Modify: `src/scanner.rs:6-9` (drop local constants, import from `session`)

- [ ] **Step 1: Add the constants to `session.rs`**

Insert immediately after `pub const TOOL_ASK_USER_QUESTION: &str = "AskUserQuestion";` (around line 10):

```rust
/// `/proc/<pid>/comm` values we accept as "this PID is still Claude Code".
/// Used by the scanner for discovery and by the registry for liveness checks,
/// so a rename here updates both paths in lockstep.
pub const CLAUDE_CODE_COMMS: &[&str] = &["claude"];

/// `/proc/<pid>/comm` values we accept as "this PID is still Codex".
pub const CODEX_COMMS: &[&str] = &["codex"];
```

- [ ] **Step 2: Update `scanner.rs` to import them**

Replace lines 6-9 of `src/scanner.rs`:

```rust
use crate::compositor::Compositor;
use crate::config::Config;
use crate::session::{
    detect_terminal, inspect_pid_cmdline, AgentKind, Session, SessionRegistry,
    CLAUDE_CODE_COMMS, CODEX_COMMS,
};
```

Then update the two usage sites in `scan_agent_processes` (around lines 50 and 52):

```rust
        if CLAUDE_CODE_COMMS.iter().any(|n| comm_lower == *n) {
            results.push((AgentKind::ClaudeCode, pid));
        } else if CODEX_COMMS.iter().any(|n| comm_lower == *n) {
            results.push((AgentKind::Codex, pid));
        }
```

- [ ] **Step 3: Build and run the full test suite to prove the refactor is behaviour-preserving**

Run:
```bash
cargo build --all-features
cargo test --all-features
```

Expected: clean build, all existing tests pass (no new tests in this task).

- [ ] **Step 4: Commit**

```bash
git add src/session.rs src/scanner.rs
git commit -m "refactor: hoist agent comm-name constants into session.rs"
```

---

### Task 2: Add `expected_comms_for(AgentKind)` helper

Central lookup from an `AgentKind` to the comms list, with explicit empty slices for window-backed agents (so any accidental call site for Cursor/WebStorm correctly reports "not liveness-checkable via /proc/comm").

**Files:**
- Modify: `src/session.rs` (add helper near the constants from Task 1)
- Test: `src/session.rs` (inline `#[cfg(test)] mod tests` block that already exists)

- [ ] **Step 1: Write the failing test**

Add inside the existing `#[cfg(test)] mod tests` block at the bottom of `src/session.rs`:

```rust
#[test]
fn expected_comms_for_cli_agents() {
    assert_eq!(expected_comms_for(AgentKind::ClaudeCode), &["claude"]);
    assert_eq!(expected_comms_for(AgentKind::Codex), &["codex"]);
}

#[test]
fn expected_comms_for_window_agents_is_empty() {
    assert!(expected_comms_for(AgentKind::Cursor).is_empty());
    assert!(expected_comms_for(AgentKind::WebStorm).is_empty());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run:
```bash
cargo test --all-features expected_comms_for
```

Expected: FAIL — "cannot find function `expected_comms_for` in this scope".

- [ ] **Step 3: Add the helper**

Insert directly below `CODEX_COMMS` in `src/session.rs`:

```rust
/// Map an `AgentKind` to the `/proc/<pid>/comm` values that identify it.
/// Returns an empty slice for window-backed agents (Cursor, WebStorm) —
/// their liveness is tracked by the compositor scan, not by `/proc`.
pub fn expected_comms_for(kind: AgentKind) -> &'static [&'static str] {
    match kind {
        AgentKind::ClaudeCode => CLAUDE_CODE_COMMS,
        AgentKind::Codex => CODEX_COMMS,
        AgentKind::Cursor | AgentKind::WebStorm => &[],
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run:
```bash
cargo test --all-features expected_comms_for
```

Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add src/session.rs
git commit -m "feat(session): add expected_comms_for(AgentKind) lookup"
```

---

### Task 3: Add `is_agent_pid_alive_with_comm` pure helper

Isolates the comm-matching logic from `/proc` I/O so it can be unit-tested without touching the filesystem or spawning processes. The public `is_agent_pid_alive` in Task 4 will read `/proc` and delegate here.

**Files:**
- Modify: `src/session.rs`

- [ ] **Step 1: Write the failing tests**

Add to the same `#[cfg(test)] mod tests` block:

```rust
#[test]
fn is_agent_pid_alive_with_comm_matches() {
    assert!(is_agent_pid_alive_with_comm("claude", AgentKind::ClaudeCode));
    assert!(is_agent_pid_alive_with_comm("codex", AgentKind::Codex));
}

#[test]
fn is_agent_pid_alive_with_comm_is_case_insensitive() {
    assert!(is_agent_pid_alive_with_comm("Claude", AgentKind::ClaudeCode));
    assert!(is_agent_pid_alive_with_comm("CODEX", AgentKind::Codex));
}

#[test]
fn is_agent_pid_alive_with_comm_trims_whitespace() {
    // /proc/<pid>/comm always has a trailing newline.
    assert!(is_agent_pid_alive_with_comm("claude\n", AgentKind::ClaudeCode));
    assert!(is_agent_pid_alive_with_comm("  claude  ", AgentKind::ClaudeCode));
}

#[test]
fn is_agent_pid_alive_with_comm_rejects_mismatch() {
    assert!(!is_agent_pid_alive_with_comm("zsh", AgentKind::ClaudeCode));
    assert!(!is_agent_pid_alive_with_comm("git", AgentKind::Codex));
    assert!(!is_agent_pid_alive_with_comm("", AgentKind::ClaudeCode));
}

#[test]
fn is_agent_pid_alive_with_comm_rejects_window_agents() {
    // Cursor/WebStorm have no comm list, so no comm can ever match.
    assert!(!is_agent_pid_alive_with_comm("cursor", AgentKind::Cursor));
    assert!(!is_agent_pid_alive_with_comm("idea", AgentKind::WebStorm));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run:
```bash
cargo test --all-features is_agent_pid_alive_with_comm
```

Expected: FAIL — "cannot find function `is_agent_pid_alive_with_comm` in this scope".

- [ ] **Step 3: Add the helper**

Insert in `src/session.rs` directly below `expected_comms_for` (from Task 2):

```rust
/// Pure helper: does a `comm` string identify the given `AgentKind`?
/// Normalises by trimming whitespace (including the trailing `\n` that
/// `/proc/<pid>/comm` always carries) and lowercasing.
pub fn is_agent_pid_alive_with_comm(comm: &str, kind: AgentKind) -> bool {
    let comm = comm.trim().to_lowercase();
    expected_comms_for(kind).iter().any(|expected| comm == *expected)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run:
```bash
cargo test --all-features is_agent_pid_alive_with_comm
```

Expected: PASS (all five tests).

- [ ] **Step 5: Commit**

```bash
git add src/session.rs
git commit -m "feat(session): add is_agent_pid_alive_with_comm pure helper"
```

---

### Task 4: Add `is_agent_pid_alive` reading `/proc/<pid>/comm`

Public API that `cleanup_dead` will call in Task 5. Reads `/proc/<pid>/comm` and delegates to the pure helper. A failed read (`/proc/<pid>` gone, permission error, etc.) means "not alive"; this matches the current `is_pid_alive` semantics for the dead-PID case and is the documented decision in the spec for the permission-error case (false positive just means re-register on next hook event).

**Files:**
- Modify: `src/session.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block:

```rust
#[test]
fn is_agent_pid_alive_rejects_non_agent_pid() {
    // PID 1 is init/systemd on Linux — alive, but comm is not "claude".
    assert!(!is_agent_pid_alive(1, AgentKind::ClaudeCode));
    assert!(!is_agent_pid_alive(1, AgentKind::Codex));
}

#[test]
fn is_agent_pid_alive_rejects_dead_pid() {
    // A very high PID is almost certainly not a live process.
    assert!(!is_agent_pid_alive(4_000_000, AgentKind::ClaudeCode));
}

#[test]
fn is_agent_pid_alive_rejects_window_agents_unconditionally() {
    // Cursor/WebStorm have no /proc-based liveness. PID 1 is alive but
    // should still report false because expected_comms_for returns [].
    assert!(!is_agent_pid_alive(1, AgentKind::Cursor));
    assert!(!is_agent_pid_alive(1, AgentKind::WebStorm));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run:
```bash
cargo test --all-features is_agent_pid_alive
```

Expected: FAIL — "cannot find function `is_agent_pid_alive` in this scope" (the tests that don't reference `_with_comm`).

- [ ] **Step 3: Replace `is_pid_alive` with `is_agent_pid_alive`**

In `src/session.rs`, delete the existing function at lines 484-487:

```rust
/// Check whether a process with the given PID is alive by probing /proc.
pub fn is_pid_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{}", pid)).exists()
}
```

Replace with:

```rust
/// Check whether a PID is still occupied by a process of the given `AgentKind`,
/// using `/proc/<pid>/comm`. Returns false when `/proc/<pid>/comm` can't be
/// read (the process has exited, the PID slot is empty, or we lack
/// permission) and when the comm name doesn't match the expected comms for
/// that kind — which is how we distinguish a live Claude session from a
/// PID that has been recycled by an unrelated process.
pub fn is_agent_pid_alive(pid: u32, kind: AgentKind) -> bool {
    let Ok(comm) = std::fs::read_to_string(format!("/proc/{}/comm", pid)) else {
        return false;
    };
    is_agent_pid_alive_with_comm(&comm, kind)
}
```

Also remove the now-unused `use std::path::Path;` import at the top of the file (line 4) if no other call site uses it. Verify with:

```bash
grep -n "Path::" src/session.rs
```

Expected: no matches. If matches exist, keep the import.

- [ ] **Step 4: Delete the obsolete `is_pid_alive_test`**

In `src/session.rs`, remove the old test at lines 702-708:

```rust
#[test]
fn is_pid_alive_test() {
    // PID 1 (init/systemd) should always be alive on Linux
    assert!(is_pid_alive(1));
    // A very high PID is almost certainly not alive
    assert!(!is_pid_alive(4_000_000));
}
```

- [ ] **Step 5: Check there are no remaining callers of `is_pid_alive`**

Run:
```bash
grep -rn "is_pid_alive" src/ tests/
```

Expected: no output. If anything shows up (a call site we missed, or a doc comment), fix it before proceeding — `cleanup_dead` is the only caller in Task 5 and will be switched there.

- [ ] **Step 6: Compile (expect one error in `cleanup_dead`)**

Run:
```bash
cargo build --all-features
```

Expected: ONE error — `cleanup_dead` in `src/session.rs` still calls `is_pid_alive` which no longer exists. That's fine; Task 5 fixes it. If you see any OTHER compile errors, stop and investigate.

- [ ] **Step 7: Do NOT commit yet**

The tree does not compile — commit happens at the end of Task 5.

---

### Task 5: Rewrite `cleanup_dead` to use the new liveness probe

Switches the registry to the agent-aware probe and exempts `window-*` sessions (whose liveness is authoritative via the compositor scan in `scanner.rs:120-129`).

**Files:**
- Modify: `src/session.rs:446-450`

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block:

```rust
#[test]
fn cleanup_dead_drops_hook_session_with_non_agent_pid() {
    // PID 1 (init) is alive but its comm is not "claude" — simulates a
    // ghost session after PID reuse.
    let registry = SessionRegistry::new();
    registry.register(Session::new(
        "11111111-2222-3333-4444-555555555555".into(),
        AgentKind::ClaudeCode,
        1,
    ));
    registry.cleanup_dead();
    assert!(registry.all().is_empty(), "ghost session should be evicted");
}

#[test]
fn cleanup_dead_drops_scan_session_with_dead_pid() {
    let registry = SessionRegistry::new();
    registry.register(Session::new(
        "scan-claude-4000000".into(),
        AgentKind::ClaudeCode,
        4_000_000,
    ));
    registry.cleanup_dead();
    assert!(registry.all().is_empty());
}

#[test]
fn cleanup_dead_retains_window_session_regardless_of_pid() {
    // Window sessions are reaped by the compositor scan; cleanup_dead
    // must not touch them even when the PID is clearly dead.
    let registry = SessionRegistry::new();
    registry.register(Session::new(
        "window-cursor-xyz".into(),
        AgentKind::Cursor,
        4_000_000,
    ));
    registry.cleanup_dead();
    assert_eq!(registry.all().len(), 1);
}
```

- [ ] **Step 2: Attempt to build — tests won't compile yet because `cleanup_dead` still references the deleted `is_pid_alive`**

Run:
```bash
cargo build --all-features
```

Expected: the same error from Task 4 Step 6 — `is_pid_alive` not found inside `cleanup_dead`.

- [ ] **Step 3: Rewrite `cleanup_dead`**

In `src/session.rs`, replace lines 446-450:

```rust
    /// Remove sessions whose PID is no longer alive.
    pub fn cleanup_dead(&self) {
        let mut map = self.sessions.write().unwrap();
        map.retain(|_, session| is_pid_alive(session.pid));
    }
```

with:

```rust
    /// Remove sessions whose PID no longer hosts a process of the expected
    /// agent kind. Window-backed sessions (`window-*` ids) are exempted —
    /// their liveness is tracked by the compositor scan in `scanner.rs`,
    /// and their `pid` belongs to a GUI app whose comm isn't in our
    /// agent-comm list.
    pub fn cleanup_dead(&self) {
        let mut map = self.sessions.write().unwrap();
        map.retain(|id, session| {
            if id.starts_with("window-") {
                return true;
            }
            is_agent_pid_alive(session.pid, session.agent)
        });
    }
```

- [ ] **Step 4: Run the full test suite**

Run:
```bash
cargo test --all-features
```

Expected: clean build, ALL tests pass — the three new `cleanup_dead_*` tests plus every pre-existing test in the crate. If any pre-existing test fails, stop and investigate; we may have missed a caller or assumption.

- [ ] **Step 5: Lint check**

Run:
```bash
cargo clippy --all-features -- -D warnings
```

Expected: no warnings. Fix any `clippy` findings introduced by the diff before committing.

- [ ] **Step 6: Commit**

```bash
git add src/session.rs
git commit -m "feat(session): evict ghost sessions via /proc/<pid>/comm match

Replace path-existence liveness check with an agent-aware comm match so
PID reuse by unrelated processes (zsh, git, cargo, …) no longer keeps
ghost Claude Code sessions in the registry. Window-backed sessions are
exempted; the compositor scan continues to reap them.

Fixes the symptom where a long-running vibewatch daemon accumulates
stale sessions in waybar and the panel over a day of work."
```

---

### Task 6: Manual verification against a live daemon

Nothing new to build — the bug is only observable end-to-end with a running daemon that has accumulated ghosts. This task confirms the fix behaves as designed in production and gives you a clean handoff point before publishing.

**Files:** none.

- [ ] **Step 1: Install the new build locally**

From the vibewatch repo:
```bash
cargo install --path . --all-features
```

Expected: `vibewatch` binary is replaced at `~/.cargo/bin/vibewatch`.

- [ ] **Step 2: Check pre-restart state**

Capture the current ghost count so you can see it drop after restart:

```bash
~/.cargo/bin/vibewatch status
```

Expected: JSON line with a `text` field starting with some count. Note it.

- [ ] **Step 3: Restart the daemon**

```bash
systemctl --user restart vibewatch.service
```

- [ ] **Step 4: Verify the count matches reality after a few seconds**

Wait ~5 s for the scanner to sweep at least once, then:

```bash
~/.cargo/bin/vibewatch status
sleep 5
~/.cargo/bin/vibewatch status
```

Expected: the count equals the number of open Claude Code terminals you actually have. If ghosts re-appear, something is wrong — check `journalctl --user -u vibewatch.service -n 50` and open an issue.

- [ ] **Step 5: Spot-check PID-reuse behaviour**

Open a scratch Claude Code terminal, note its PID (`pgrep -af "^/.*claude"` or check the panel), then quit that terminal. Within ~3 s the count should decrement — before, it would have stuck until the PID slot was freed or overwritten by a same-named process. Nothing to commit here; this is an observability check.

- [ ] **Step 6: Push**

If everything above looks right:
```bash
git push origin main
```

---

## Self-Review

**Spec coverage** (checked against `docs/superpowers/specs/2026-04-22-stale-session-eviction-design.md`):

- "Evict ghost hook-registered sessions within one scanner sweep" → Tasks 4 + 5. `cleanup_dead` is called at the top of the 3 s loop; `is_agent_pid_alive` returns false when comm mismatches, so eviction happens within one sweep.
- "Evict ghost scanner-discovered sessions (`scan-*`) under the same rule" → Task 5 step 1 includes `cleanup_dead_drops_scan_session_with_dead_pid` and the rewritten `cleanup_dead` applies the comm check to all non-`window-*` ids.
- "Leave window-based sessions untouched" → Task 5 step 1 includes `cleanup_dead_retains_window_session_regardless_of_pid` and the `window-` prefix guard in the rewritten `cleanup_dead`.
- "Zero new configuration" → no config touched.
- "Move constants from `scanner.rs` to `session.rs`" → Task 1.
- "Replace `is_pid_alive` with `is_agent_pid_alive`; delete `is_pid_alive` and its test" → Task 4 steps 3, 4, 5.
- "No structural change in `scanner.rs`" → Task 1 only imports; no logic change.
- "No log line on eviction" → no task adds one.
- "No panel/waybar changes" → no task touches `panel/` or `waybar.rs`.
- "Unit-test the pure comm-match helper; don't refactor `/proc` root to be injectable" → Tasks 2, 3, 4 follow this split.

**Placeholder scan:** none ("TBD", "TODO", "handle edge cases", "similar to", "etc." in prose only, not in prescriptive steps).

**Type consistency:**
- `expected_comms_for(AgentKind) -> &'static [&'static str]` — used identically in Tasks 2, 3 helper, 3 test, 4.
- `is_agent_pid_alive_with_comm(&str, AgentKind) -> bool` — Tasks 3 and 4 use the same signature.
- `is_agent_pid_alive(u32, AgentKind) -> bool` — Tasks 4 and 5 use the same signature.
- Constants `CLAUDE_CODE_COMMS`, `CODEX_COMMS` — introduced in Task 1, referenced unchanged in Task 2.
- Existing `Session::new(String, AgentKind, u32)` — Task 5 tests use it the same way the existing registry tests do (`session.rs:653, 664, 696`).
