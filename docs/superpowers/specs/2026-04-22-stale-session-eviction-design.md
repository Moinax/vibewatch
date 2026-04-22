# Stale session eviction via `/proc/<pid>/comm` check

## Problem

`SessionRegistry::cleanup_dead` retains a session iff `/proc/<pid>` exists
(`session.rs:447-450`, via `is_pid_alive`). On a long-running daemon this lets
ghost sessions accumulate: a Claude Code process exits, its PID is later
reused by an unrelated process (zsh, git, cargo, …), the path test still
passes, and the session is never reaped. Waybar count and panel both show
agents that no longer exist.

Observed: five distinct Claude Code session UUIDs over ~8 hours, only one
actually live. Waybar reports "4" active agents when the user has 2 terminals
open.

The existing scanner sweep runs every 3 s and is the right place to fix this;
we don't need a new task or a timer.

## Goals

- Evict ghost hook-registered sessions (UUID ids) within one scanner sweep
  (~3 s) of their PID being freed or reused by a non-agent process.
- Evict ghost scanner-discovered sessions (`scan-*` ids) under the same rule.
- Leave window-based sessions (`window-*`, Cursor/WebStorm) untouched; the
  compositor scan in `scanner.rs:120-129` is already authoritative for them.
- Zero new configuration.

## Non-goals

- Idle-activity timeout. Considered and dropped: it adds a config knob, can
  mis-evict a live-but-idle session, and its latency is worse (minutes vs.
  seconds). If the Claude→Claude-same-PID case below ever bites in practice,
  revisit.
- Detecting zombies (`<defunct>`). `comm` still reports the original name
  until the parent reaps; typically sub-second and not worth complicating.
- Changing hook behaviour. No new Claude Code hook is required.

## Approach

Harden the liveness probe so "alive" means **the PID slot still holds a
process that matches the agent we registered**, not just "the PID slot is
occupied".

`/proc/<pid>/comm` is authoritative for the current executable name and
already matches the constants the scanner uses to *discover* agents
(`scanner.rs:8-9`):

```rust
const CLAUDE_CODE_NAMES: &[&str] = &["claude"];
const CODEX_NAMES:       &[&str] = &["codex"];
```

Verified on the target system: running Claude Code shows `comm=claude`.
`pid_max` on modern Linux (Fedora/Arch default: 4 194 304) makes
Claude→Claude collision on the exact same PID rare enough to ignore.

## Design

### `session.rs`

Replace

```rust
pub fn is_pid_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{}", pid)).exists()
}
```

with an agent-aware variant:

```rust
pub fn is_agent_pid_alive(pid: u32, kind: AgentKind) -> bool {
    // PID slot must still exist.
    let comm_path = format!("/proc/{}/comm", pid);
    let Ok(comm) = std::fs::read_to_string(&comm_path) else {
        return false; // /proc entry gone → process dead
    };
    let comm = comm.trim().to_lowercase();
    expected_comms_for(kind).iter().any(|n| &comm == n)
}

fn expected_comms_for(kind: AgentKind) -> &'static [&'static str] {
    match kind {
        AgentKind::ClaudeCode => &["claude"],
        AgentKind::Codex      => &["codex"],
        // Window-backed agents are reaped by the compositor scan; return
        // an empty slice so the registry never tries to comm-check them
        // (it must not, their PID belongs to the GUI app, not a CLI).
        AgentKind::Cursor | AgentKind::WebStorm => &[],
    }
}
```

Move the `CLAUDE_CODE_NAMES` / `CODEX_NAMES` constants from `scanner.rs` into
`session.rs` so `scanner.rs::scan_agent_processes` and `expected_comms_for`
share a single source of truth. The scanner keeps using them for discovery.

`cleanup_dead` becomes:

```rust
pub fn cleanup_dead(&self) {
    let mut map = self.sessions.write().unwrap();
    map.retain(|id, s| {
        // Compositor-backed sessions are reaped by the window scan; skip.
        if id.starts_with("window-") {
            return true;
        }
        is_agent_pid_alive(s.pid, s.agent)
    });
}
```

`is_pid_alive` has no other callers (only `cleanup_dead` and its own test);
delete it along with its test, since `is_agent_pid_alive` subsumes it.

### `scanner.rs`

No structural change. The existing `registry.cleanup_dead()` at the top of
each 3 s loop now does the right thing. Update the import to pull the
`*_NAMES` constants from `session.rs` rather than redeclaring.

No eviction log line. Existing scanner code logs nothing on `cleanup_dead`;
keep it that way. Operators can observe evictions via waybar count changes
and panel updates.

### `ipc.rs` / `notify.rs`

No changes. The `pid` field carried by hook events is already the Claude
Code PID (`notify.rs:354` — `parent_pid()`), so the registry has what it
needs.

### Panel / waybar

No changes. `cleanup_dead` is already followed by `status_notify.notify_waiters()`
in the scanner loop (`scanner.rs:158`); the waybar subscriber will push a new
JSON line with the decremented count, and the panel rebuilds its rows from
`registry.all()` on the same signal. Silent eviction, no UI work required.

## Edge cases

| Case | Behaviour |
|---|---|
| Claude exits, PID freed | `/proc/<pid>/comm` read fails → evict next sweep. |
| Claude exits, PID reused by `zsh`/`git`/anything non-agent | comm ≠ "claude" → evict next sweep. |
| Claude exits, PID reused by another Claude Code | comm == "claude" → ghost UUID survives. New Claude's `SessionStart` hook registers its own UUID separately. Rare with `pid_max=4M`; accepted residual. |
| Zombie (`<defunct>`) Claude | comm still reports "claude" briefly; evicted when parent reaps. Accepted. |
| `/proc/<pid>/comm` permission error | read fails → session evicted. Accepted: on a single-user system this doesn't happen for the user's own PIDs; a false positive just means the session re-registers on its next hook event. |
| `window-*` session for Cursor/WebStorm | skipped by `cleanup_dead`; compositor scan continues to reap. |

## Testing

Unit tests in `session.rs`:

- `is_agent_pid_alive` returns `true` for the current process's PID when
  called with the `AgentKind` whose comm list contains the test binary's
  comm. Simplest: pick `AgentKind::ClaudeCode` and temporarily set
  `/proc/self/comm`? That's writable for the owning process via
  `prctl(PR_SET_NAME)`, but pulling in `libc` just for this is overkill.
  Alternative: construct a stub that reads from a tempdir-backed
  `/proc/<pid>/comm` path — requires making the `/proc` root injectable,
  which is more refactor than the fix warrants.
  **Pragmatic choice:** add a narrow, non-public helper
  `is_agent_pid_alive_with_comm(comm: &str, kind: AgentKind) -> bool` that
  takes the comm string directly; unit-test that. Keep
  `is_agent_pid_alive` as the `/proc`-reading wrapper, covered only by the
  existing integration test.

- `cleanup_dead` retains window sessions regardless of PID liveness; drops
  hook/scan sessions whose PID slot is empty; drops hook/scan sessions whose
  PID is alive but comm mismatches. Use a registry populated with fake
  sessions pointing at PID 1 (init, comm=`systemd`) and an obviously-dead
  PID (e.g. `u32::MAX`) to get deterministic coverage without spawning
  processes.

Integration coverage in `tests/integration_test.rs` if a natural slot
exists; otherwise unit tests are sufficient.

## Rollout

Single patch, no migration, no config. Users on `main` pick it up after the
next `./manage.sh update` (which restarts `vibewatch.service` on SHA change,
`manage.sh:80`). Existing ghost sessions in a running daemon's memory clear
on the first sweep after restart.

## Follow-ups (out of scope)

- If the Claude→Claude-same-PID case ever shows up in the wild, add a
  lightweight idle-activity timeout as a second layer. Cost is one epoch
  field on `Session` and one additional `retain` pass; design already
  sketched in brainstorming.
- Consider surfacing the running agent count and eviction events on a debug
  subcommand (`vibewatch debug registry`) for future diagnosis. Not needed
  now.
