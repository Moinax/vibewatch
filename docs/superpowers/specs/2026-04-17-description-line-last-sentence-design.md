# Description line: always show the last sentence (user or agent)

**Date:** 2026-04-17
**Component:** `src/panel/session_row.rs`, `src/session.rs`, `src/main.rs`, new `src/transcript.rs`

## Problem

Today the description line in a session row shows a mix of content depending on status:

- `You: "<prompt>"` if there is a `last_prompt` and the status is not `WaitingApproval`/`Stopped`.
- Otherwise a status-driven string: `Executing <tool>`, `After <tool>`, `Needs approval: <tool>`, `Idle`, `Stopped`.

The user-facing effect is that the row often blanks out to generic `Idle` or `Thinking...` right when the most interesting signal (what the agent just said) exists but is not captured anywhere. Hooks never carry assistant text, so the daemon currently has no way to show it.

## Goal

The description line always shows **the most recent sentence exchanged in the session**, attributed to either the user or the agent:

- `You: "<user prompt>"` when the user spoke most recently.
- `Claude: "<assistant text>"` / `Codex: "<assistant text>"` when the agent spoke most recently.
- Current status fallback (`Idle`, `Stopped`, `Thinking...`) when neither has been captured yet.

The action line (second, optional line) keeps its current role: live tool activity (`Writing config.rs`) or the trailing tool action while the agent is `Thinking`. Status-only strings like `Needs approval: Bash` move out of the description and onto the action line so the description is never hijacked.

## Non-goals

- Real-time streaming of assistant text as it is produced.
- Sentence segmentation (we pick the last non-empty line, not the last `.`/`!`/`?`-delimited sentence).
- Rendering of `thinking` blocks, code fences, or multi-line answers.
- Cursor / WebStorm assistant text (no accessible transcript).

## Approach

Hook-triggered transcript read. Whenever a hook fires for a session, the daemon reads the last assistant text line from that session's transcript JSONL file and stores it on the `Session`. No filesystem watchers, no new background tasks.

Alternatives considered:

- **inotify watcher per session** — real-time but introduces per-session tokio tasks, fd pressure, and a `notify`-crate dependency. Overkill for a panel that refreshes at most every second.
- **On-demand read at render time** — simplest code but hits disk on every panel refresh and every Waybar text/class emission, for every session.

## Data model

### `Session` additions (`src/session.rs`)

```rust
pub struct Session {
    // ...existing fields...
    pub last_agent_text: Option<String>,
    pub last_agent_text_at: Option<u64>,   // unix epoch seconds, set when last_agent_text changes
    pub last_prompt_at: Option<u64>,        // unix epoch seconds, set when last_prompt is set
    pub transcript_path: Option<PathBuf>,   // cached per session once resolved
}
```

All four are serialized (consistent with existing fields).

## New module: `src/transcript.rs`

```rust
pub fn read_last_assistant_line(
    agent: AgentKind,
    session_id: &str,
    cached_path: &mut Option<PathBuf>,
) -> Option<String>;
```

Behavior:

- Resolves and caches the transcript path on first successful read:
  - `AgentKind::ClaudeCode` → walk `~/.claude/projects/*/<session_id>.jsonl` (same search as the existing `read_transcript_name`).
  - `AgentKind::Codex` → `~/.codex/sessions/<session_id>.jsonl` (verify exact layout during implementation; fall back to walking `~/.codex/sessions/**/<session_id>.jsonl` if nested).
  - `AgentKind::Cursor` / `AgentKind::WebStorm` → returns `None` immediately.
- Reads the file line-by-line from the end (bounded lookback, e.g. last 256 KiB, to stay cheap on long sessions). For each line, parses JSON; ignores malformed lines.
- Claude schema: a line with `role == "assistant"` whose `content` is an array; collect all items where `type == "text"`, concatenate their `text` fields in order.
- Codex schema: the equivalent — identify during implementation.
- From the concatenated text: strip trailing whitespace, drop lines that are *only* code fences (`` ``` `` / `` ```lang ``), trim each remaining line, and return the **last non-empty line**.
- If no assistant message is found, or the line would be empty/code-fence-only, returns `None`.

## Event wiring (`src/main.rs`)

Add transcript reads at two points:

- **`PostToolUse`** — after the existing status transition to `Thinking`, call `transcript::read_last_assistant_line(...)`. If `Some(text)`, set `last_agent_text = Some(text)` and `last_agent_text_at = now`.
- **`Stop`** — same transcript read.

**`UserPromptSubmit`** keeps its existing behavior and additionally sets `last_prompt_at = now`.

`SessionStart`, `PreToolUse`, `PermissionRequest`, `PermissionDenied` are unchanged.

## Rendering (`src/panel/session_row.rs`)

Replace `status_description(session)` with a pure function `describe(session) -> String` that follows:

```
let user_at  = session.last_prompt_at;
let agent_at = session.last_agent_text_at;

match (session.last_prompt.as_deref(), session.last_agent_text.as_deref(), user_at, agent_at) {
    (Some(p), None, _, _)                   => render("You", session.agent, Speaker::User, p),
    (None, Some(a), _, _)                   => render(&agent_prefix, session.agent, Speaker::Agent, a),
    (Some(p), Some(a), Some(pu), Some(au))  => if pu > au { user } else { agent },
    // neither captured yet → status fallback
    _ => match session.status {
        Idle | Running   => "Idle".into(),
        Stopped          => "Stopped".into(),
        Thinking         => "Thinking...".into(),
        Executing        => "Working...".into(),
        WaitingApproval  => "Awaiting approval".into(),
    }
}
```

Where `render` produces `"{prefix}: \"{truncated}\""`, truncation bound ≈60 chars (keeping room for the prefix on the ellipsized label).

`agent_prefix` is `session.agent.short_name()` (`Claude`, `Codex`, `Cursor`, `WS`).

`action_line(session)` keeps its current behavior for live tool calls (present tense) and for the previous tool during `Thinking` (past tense). It is extended so that `WaitingApproval` emits `Needs approval: <tool>` (replacing the hijack of the description line that exists today). During `Executing`, the description shows `Working...` (or user/agent text if available), while the action line continues to show the live `Writing config.rs` / `cargo build` detail — no duplication.

## Error handling / edge cases

- Missing transcript file → `read_last_assistant_line` returns `None`; description falls back to status text. No panic, no error log on the hot path.
- Transcript line is not valid JSON → skip, keep scanning earlier lines.
- Assistant message's last line is a code fence only → `None`; falls back.
- Transcript not yet discovered (session just started, hook fired but no assistant message yet) → `None`; falls back.
- Very long transcripts → bounded reverse-read (last 256 KiB) so we never scan the whole file.

## Testing

### `transcript.rs` unit tests (with fixture JSONL files)

Fixtures live under `tests/fixtures/transcripts/{claude,codex}/`:

- `assistant_ends_with_text.jsonl` — expect last non-empty line of the last text block.
- `assistant_ends_with_tool_use.jsonl` — last `assistant` message has no text block; expect `None`.
- `assistant_multi_text_blocks.jsonl` — concatenation is exercised; last line across all text blocks wins.
- `empty_transcript.jsonl` — `None`.
- `malformed_lines.jsonl` — malformed lines interleaved; parser skips them and returns the right result.
- `ends_with_code_fence.jsonl` — last line is `` ``` ``; expect the last non-fence line, or `None` if none.
- `cursor_webstorm` → no fixture; the `AgentKind` branch is tested by a direct call returning `None`.

### `session_row.rs` render tests

Extract description rendering into a pure helper, e.g. `describe(session: &Session) -> String`. Cover:

- User newest → `You: "..."`.
- Agent newest → `Claude: "..."`.
- Both present, user timestamp later → user wins.
- Both present, agent timestamp later → agent wins.
- Equal timestamps → agent wins (the assistant spoke after consuming the prompt).
- Neither captured yet, `Idle` → `Idle`.
- Neither captured, `Executing` → `Working...`.
- Neither captured, `WaitingApproval` → `Awaiting approval`.
- Long text → truncated with ellipsis, still prefix-attributed.

### Integration smoke

Add a test to `main.rs`-level event plumbing (or a dedicated module test) that:

1. Feeds a `UserPromptSubmit`, then `PostToolUse`, with a fixture transcript path.
2. Asserts the resulting `Session` has both `last_prompt_at` and `last_agent_text_at` set, with the expected text.

## Out of scope / follow-ups

- Codex transcript schema verification — confirm path and JSON shape during implementation; if the schema differs materially, file a follow-up for Codex-specific parsing polish.
- Streaming updates (inotify watcher).
- Rendering code blocks gracefully (e.g. showing `` ```diff … ``` `` with an icon rather than its last line).
- Per-agent description prefix customization.
