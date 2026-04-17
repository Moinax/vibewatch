# Widget Approval UI — Design Spec

**Date:** 2026-04-17
**Status:** Draft (awaiting user review)
**Scope:** Claude Code only. Codex has no equivalent hook mechanism and is out of scope.

## Goal

Let the user accept or deny a Claude Code tool-approval request directly from the vibewatch panel, without switching to the terminal where the agent runs. Inspired by VibeIsland.

## User-facing behaviour

1. When Claude needs permission for a tool, the vibewatch panel auto-shows. The row for that session switches into an **approval card** that displays:
   - Session name + existing badges (agent / terminal / elapsed time)
   - The user's or agent's last sentence (existing description line)
   - A **detail line**: tool name + specific command/file (e.g. `Bash: rm -rf node_modules` or `Edit: src/main.rs`)
   - Two buttons: **Accept** (green) and **Deny** (red)
2. Clicking **Accept** → Claude proceeds with the tool call.
3. Clicking **Deny** → Claude is told the tool was denied and falls back to asking what to do instead.
4. If the user never clicks, after ~580 s the hook exits with `permissionDecision: "ask"` and Claude's normal terminal prompt appears as a fallback. Nothing is ever auto-approved without an explicit click.
5. Multiple concurrent approvals (from different sessions) render independently — each session row shows its own Accept/Deny buttons.
6. Codex sessions retain the existing read-only `Needs approval: <tool>` action line with no buttons.

## Mechanism

Claude Code's `PreToolUse` hook (and, where applicable, its permission-specific hook) can write a JSON decision to stdout that Claude honours:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow" | "deny" | "ask",
    "permissionDecisionReason": "..."
  }
}
```

Claude waits up to 600 s for the hook to exit. This gives us a blocking channel: the hook can open a Unix-socket connection to the vibewatch daemon, block reading a response line, and then write the right JSON to stdout.

### Hook boundaries

- **Existing `pre-tool-use` hook** stays fire-and-forget. It only updates the UI (populates `current_tool` + `tool_detail`). No behavioural change here.
- **New hook** is registered against Claude's permission-request event and invoked as `vibewatch notify permission-request --agent claude-code`. It blocks until the user decides, or falls back after timeout.

Keeping the two hooks separate means auto-approved tool calls incur no round-trip latency — only calls that would have prompted the user go through the daemon.

### End-to-end flow

1. Claude decides tool X needs permission → fires the permission-request hook.
2. The hook process:
   a. Reads stdin, parses tool name + `tool_input`.
   b. Generates a fresh `request_id` (UUID v4).
   c. Connects to the daemon socket, writes a JSON `PermissionRequest { session_id, request_id, tool, detail, tool_input, pid }` line, keeps the stream open.
   d. Blocks reading one response line with a ~580 s read-timeout.
3. Daemon, on receiving `PermissionRequest`:
   - Looks up the session by `session_id` (with PID-based `get_or_adopt` fallback, same as existing hooks).
   - Sets `session.status = WaitingApproval` and fills `session.pending_approval = Some(PendingApproval { request_id, tool, detail })`.
   - Inserts `(request_id → (stream, oneshot::Sender<bool>))` into the `ApprovalRegistry`.
   - Calls the existing "toggle panel" code path to auto-show the panel if hidden.
4. Panel rebuild sees `pending_approval.is_some()` and renders the Accept/Deny buttons on that row. Other rows continue as normal.
5. User clicks **Accept** or **Deny**. The button handler connects to the daemon socket and sends an `ApprovalDecision { request_id, approved }` event, exactly like any other IPC event today.
6. Daemon, on receiving `ApprovalDecision`:
   - Looks up and removes the `ApprovalRegistry` entry for `request_id`.
   - Writes `{"approved": true|false}\n` to the held stream, flushes, drops the stream.
   - Clears `session.pending_approval` and restores `session.status` based on whether any tool is still active.
7. Hook reads the response line, maps to Claude's decision JSON, writes to stdout, exits. Claude proceeds.
8. **Timeout path**: if the hook's read-timeout fires before step 7, it writes `{"permissionDecision":"ask"}` and exits. Claude's terminal prompt takes over. Daemon detects the dropped stream (broken pipe on its next write, or periodic reaper — see Failure modes) and clears the pending approval.

## Data model changes

### `src/session.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingApproval {
    pub request_id: String,
    pub tool: String,
    pub detail: Option<String>,
}

pub struct Session {
    // ...existing fields...
    pub pending_approval: Option<PendingApproval>,
}
```

`PendingApproval` is `Serialize`-safe so it shows up in `vibewatch status` JSON for debugging. It intentionally does **not** carry the stream or sender — those live outside the serializable state.

### `src/ipc.rs`

```rust
pub enum InboundEvent {
    // ...existing variants...
    PermissionRequest {
        session_id: String,
        request_id: String,
        tool: String,
        detail: Option<String>,
        pid: Option<u32>,
    },
    ApprovalDecision {
        request_id: String,
        approved: bool,
    },
}
```

The existing `PermissionRequest` variant is extended — the older fire-and-forget caller (if any) keeps working because `request_id` default is generated on the hook side. `PermissionDenied` is kept for backwards compatibility with any existing callers but becomes unused in the new flow.

### Daemon (`src/main.rs`)

New type, held alongside `SessionRegistry`:

```rust
type ApprovalRegistry = Arc<Mutex<HashMap<String, ApprovalEntry>>>;

struct ApprovalEntry {
    stream: tokio::net::UnixStream,
    created_at: std::time::Instant,
}
```

The registry owns the open hook stream. On `ApprovalDecision`, the daemon removes the entry, writes the response, drops the stream. On session drop or explicit cancellation, pending entries are reaped (see Failure modes).

## UI changes

### `src/panel/session_row.rs`

When `session.pending_approval.is_some()`:

1. The existing action-line label renders the detail: `{tool}: {detail}` (reusing `describe_tool(tool, detail, true)`).
2. Below the action line, insert a horizontal box with two `gtk::Button` widgets: **Accept** and **Deny**.
3. Each button's click handler is wired to:
   - Connect to `config.socket_path()` via Tokio on a blocking thread.
   - Send `InboundEvent::ApprovalDecision { request_id, approved }`.
   - No UI mutation on the client side — the daemon's state update will propagate back through the normal 500 ms poll and re-render the row without buttons.

When `pending_approval` is `None`, no buttons are rendered — the existing read-only action line is shown.

### Styling (`assets/style.css`)

- `.approval-accept` — Catppuccin green (`#a6e3a1`), subtle border, hover glow.
- `.approval-deny` — Catppuccin red (`#f38ba8`), same shape, hover glow.
- Buttons are compact (roughly 60 × 22 px) to fit inside a 360 px wide row.
- Gap between buttons: 6 px. Row bottom padding expands slightly when buttons are present.

## Auto-show

When the daemon receives a `PermissionRequest`, it dispatches a GTK-main-thread callback that calls `window.set_visible(true)` followed by `window.present()`. This mirrors the *show* half of the existing `TogglePanel` handler but is always unconditional (never hides an already-visible panel). Panel stays visible until the user toggles it away via the Waybar module or a subsequent `TogglePanel` event. The implementation should share a single `show_panel()` helper so both paths stay in sync.

## Timeout + fallback

The hook's blocking read uses a single `tokio::time::timeout` with a 580 s budget (20 s under Claude's 600 s ceiling to leave room for stdout write). On timeout:

- Hook writes `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}` to stdout.
- Hook exits `0`.
- Claude's terminal prompt appears as a fallback.

On the daemon side, a background task runs every 30 s and reaps any `ApprovalEntry` older than 580 s — writing to the stream (expected to fail) and clearing the session's `pending_approval`. This keeps ghost entries from accumulating if a hook was killed uncleanly.

## Failure modes

| Failure | Behaviour |
| --- | --- |
| Daemon not running when hook fires | `UnixStream::connect` errors → hook writes `{"permissionDecision":"ask"}` immediately, terminal prompt takes over. |
| Compositor refuses `window.present()` | Panel stays hidden; user manually clicks Waybar module; buttons are still reachable from the Waybar-triggered open. Hook still times out safely. |
| User kills hook (Ctrl-C on Claude) | Stream closes; daemon's eventual write fails → entry reaped, `pending_approval` cleared. |
| Two approvals from same session simultaneously | Claude waits for one tool call at a time per session, so this should not happen in practice. If it does, the daemon overwrites `session.pending_approval` with the newer request; the older `ApprovalRegistry` entry stays alive until it times out (580 s) and is reaped. The widget only shows the latest one. |
| `ApprovalDecision` arrives for an unknown `request_id` | Daemon logs and ignores. Harmless (e.g. button click arrived after a timeout-reap). |

## Out of scope

- Codex approval integration (no hook mechanism).
- Auto-approve allowlists configured in vibewatch (let Claude's own `permissions` config handle that).
- Approval history / audit log UI.
- Three-button VibeIsland-style "Allow always". Can be added later; the event schema is forward-compatible if we add an `ApprovalScope` field to `ApprovalDecision`.
- Keyboard shortcuts for Accept/Deny inside the panel (nice-to-have; defer).

## Decision log

- **Accept / Deny only** (not three buttons) — matches user preference for the simplest option.
- **Auto-show panel**, no sound — user preference.
- **Fall back to terminal prompt on timeout** — never auto-approve silently.
- **Separate blocking hook, not extending `pre-tool-use`** — avoids round-trip latency on auto-approved tool calls.
- **`request_id` UUID** — allows multiple outstanding approvals (even across sessions) and makes the `ApprovalDecision` event idempotent-ish (stale IDs are ignored).
- **`PendingApproval` is serializable** — shows up in `vibewatch status` for debugging; stream + timer stay outside.
