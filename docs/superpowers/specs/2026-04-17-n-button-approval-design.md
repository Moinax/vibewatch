# N-Button Approval UI — Design Spec

**Date:** 2026-04-17
**Status:** Draft (follow-up to `2026-04-17-widget-approval-ui-design.md`)
**Scope:** Claude Code only.

## Goal

Replace the fixed Accept / Deny pair in the vibewatch approval card with the exact set of buttons Claude Code's terminal TUI would have shown — typically `Yes`, one or more `Yes, and allow X for Y` suggestions, and `No`. The user's click binds Claude Code's decision just like the terminal dialog would.

## Motivation

Today the widget shows only two buttons even though Claude Code's terminal dialog often offers three or more:

```
Do you want to proceed?
  1. Yes
  2. Yes, allow reading from .claude/ during this session
  3. No
```

The third option in particular — "allow … for this session" — is a rule-based decision the user loses by clicking the widget instead of the terminal.

## Payload we already receive

The `PermissionRequest` hook's stdin carries a `permission_suggestions` array. Empirically observed shape:

```json
{
  "session_id": "...",
  "cwd": "...",
  "permission_mode": "acceptEdits",
  "hook_event_name": "PermissionRequest",
  "tool_name": "Read",
  "tool_input": { "file_path": "/home/moinax/.claude/settings.json" },
  "permission_suggestions": [
    {
      "type": "addRules",
      "rules": [ { "toolName": "Read", "ruleContent": "//home/moinax/.claude/**" } ],
      "behavior": "allow",
      "destination": "session"
    }
  ]
}
```

Each suggestion has:

- `type`: always `"addRules"` in what we've seen.
- `rules`: array of `{toolName, ruleContent}` — the glob/pattern to add.
- `behavior`: `"allow"` (observed) — in principle `"deny"` could appear as a "never allow" button.
- `destination`: `"session"` (observed) — in principle `"project"` or `"user"` could also appear.

## Data model

### `PermissionSuggestion` (new, serializable)

```rust
pub struct PermissionSuggestion {
    pub r#type: String,                // "addRules"
    pub rules: Vec<PermissionRule>,
    pub behavior: String,              // "allow" | "deny"
    pub destination: String,           // "session" | "project" | "user"
}

pub struct PermissionRule {
    pub tool_name: String,
    pub rule_content: String,
}
```

### `ApprovalChoice` (new, serializable)

One entry per button rendered in the widget.

```rust
pub struct ApprovalChoice {
    pub label: String,                            // UI text
    pub behavior: String,                         // "allow" | "deny"
    pub suggestion: Option<PermissionSuggestion>, // None for plain Yes/No
}
```

### `PendingApproval` (extended)

Replaces today's `{request_id, tool, detail}` shape with a version that carries the choices:

```rust
pub struct PendingApproval {
    pub request_id: String,
    pub tool: String,
    pub detail: Option<String>,
    pub choices: Vec<ApprovalChoice>,
}
```

## Wire protocol

### `InboundEvent::PermissionRequest` (extended)

Add `permission_suggestions: Vec<PermissionSuggestion>` (defaults to empty when absent for backwards compatibility with older hook binaries).

### `InboundEvent::ApprovalDecision` (replaced)

Current `{ request_id, approved: bool }` is replaced by index-based choice selection:

```rust
ApprovalDecision {
    request_id: String,
    choice_index: usize,
}
```

The widget knows what buttons it rendered and reports which one was clicked.

### Hook-to-daemon response line

Today: `{"approved":true|false}\n`. New shape:

```json
{"behavior":"allow","suggestion":{...}|null}
```

The daemon looks up the selected `ApprovalChoice`, writes this line on the stashed socket. The hook reads it and translates to Claude Code's decision JSON.

## Hook output to Claude Code

For every choice, the hook emits:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PermissionRequest",
    "decision": {
      "behavior": "<allow|deny>",
      "reason": "via vibewatch widget"
    }
  }
}
```

**Session-rule attachment (phase 1 — best effort).** When the chosen choice has a `suggestion`, the hook also writes a nested field under `decision` matching the suggestion's shape verbatim. Candidate field names to probe empirically, in order: `addRules`, `suggestion`, `rule`, `suggestedRule`. We'll deploy with the most likely (`suggestion`) and rely on the diagnostic log to tell us whether Claude Code applied the rule. If none work, fall back to phase 2.

**Session-rule attachment (phase 2 — fallback, only if phase 1 fails).** The daemon writes the suggestion's rules into `~/.claude/settings.local.json` before returning `allow`. Claude Code reloads that file on subsequent calls and the rule takes effect. Out of scope for the first iteration; document the fallback and revisit only if phase 1 doesn't work.

## UI changes (`src/panel/session_row.rs`)

- `build_approval_bar(request_id)` renamed to `build_choice_bar(request_id, choices)`.
- For each `choice` in `choices`, create one `gtk::Button` with the choice's label as the button text.
- Buttons styled by semantics, not by position: `.approval-accept` on any choice with `behavior == "allow"`, `.approval-deny` on any `deny` choice. Differentiate the per-session rule suggestions from the plain "Yes" via a new `.approval-scope` class (softer green).
- Wrap buttons in a horizontal `gtk::Box` with `spacing = 6`. If the row gets too wide, GTK will naturally reflow; we won't try to be cleverer.
- Click handler: `send_approval_decision(request_id, choice_index)` — same shape as today, with `choice_index: usize` instead of `approved: bool`.

## Rendering labels

Suggestion labels are built on the hook side (the daemon doesn't need to know about globs). Format:

- `Yes, allow <tool_name> for <rule_content> (<destination>)` — e.g. `Yes, allow Read for ~/.claude/** (session)`.
- If a suggestion has multiple rules, join with `+`.
- `ruleContent` is stripped of leading `//` and trailing duplicates before rendering.

Plain "Yes" is the literal string `"Yes"`; plain "No" is `"No"`.

## Backwards compatibility

- `ApprovalDecision` changes its on-wire shape from `{approved}` to `{choice_index}`. This breaks the old format. Since both the hook binary and the panel button code ship in the same vibewatch binary, a single-version bump is fine — no external consumers.
- `PendingApproval`'s new `choices` field is added with `#[serde(default)]` so older snapshots in `vibewatch status` output stay parseable.

## Failure modes

- **Hook binary too old** (doesn't send `permission_suggestions`): daemon defaults to empty suggestions → UI renders 2 buttons (Yes / No). Matches today's behavior.
- **No suggestions in payload**: UI renders 2 buttons. Degrades gracefully.
- **User clicks a session-rule button but Claude Code ignores the rule attachment**: the tool call is still allowed once. User sees the widget fire again the next time, which is wrong-but-safe. We log what we sent so we can iterate toward phase 2.
- **Unknown `destination` values** (e.g. Claude Code adds `"worktree"` later): renders the raw string inside the parenthesis. No crash.

## Cleanup

This spec also removes two pieces of scaffolding from the previous feature:
- The `std::fs::write("/tmp/vibewatch-permission-request.json", ...)` hook-side dump added for debugging.
- The diagnostic `eprintln!` lines on the daemon side (`recv PermissionRequest ...`, `wrote decision line ...`). Keep only the error-path logs.

## Out of scope

- Rendering the `preview` field for suggestions (visual preview panes per VibeIsland).
- Keyboard shortcuts for buttons.
- AskUserQuestion-in-widget (separate deferred feature).
- Dead `PermissionDenied` code in `ipc.rs` / `notify.rs` (housekeeping; not needed for this feature).

## Decision log

- **`choice_index` over `{behavior, suggestion}` in `ApprovalDecision`**: keeps the daemon as the source of truth for the rendered choices; the panel just reports which button was pressed.
- **Hook builds labels, not the daemon**: the hook has the cleanest view of the raw payload and only needs to do it once per invocation.
- **Ship phase 1 (empirical hook output) first**: the docs don't show the decision-attached-rule schema, so probing in prod is faster than blocking on a docs fetch that already came back incomplete.
