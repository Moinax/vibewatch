# vibewatch

**A status bar and glanceable overlay for your AI coding agents — on Wayland.**

vibewatch is an open-source alternative to [Vibe Island](https://vibeisland.dev/) built for Linux, Hyprland, and Niri. It listens to your agents' hooks in real time and gives you a single place to see what every Claude Code or Codex session is doing — and, when one stops to ask for permission, lets you **approve or deny right from the overlay** instead of hunting for the right terminal tab.

```
┌──────────────────────────────────────────────┐
│  ●  dotfiles              claude-code  kitty │
│     Waiting for approval — Bash: rm -rf …    │
│     [ Yes ]  [ Yes, allow rm ]  [ No ]       │
├──────────────────────────────────────────────┤
│  ●  vibewatch             claude-code  kitty │
│     Thinking…                                │
└──────────────────────────────────────────────┘
```

## Why

Running multiple AI agents in parallel is great — until one quietly blocks on a `rm -rf` approval while you're heads-down in another window, and the whole pipeline stalls for ten minutes before you notice.

vibewatch fixes that. One glance at your bar tells you which sessions are running, thinking, or blocked. One click on the overlay answers the prompt. No more Alt-Tab roulette.

## Features

- **Live session tracking** — detects running agent processes and follows their state via hook events (thinking, running a tool, waiting for approval, stopped).
- **Waybar module** — compact JSON module with per-agent icons and click-to-open behavior.
- **GTK4 overlay panel** — layer-shell popup with a card per session: name, agent, terminal, current tool, elapsed time.
- **Click-to-approve** — Claude Code permission prompts are rendered as real buttons inside the overlay, forwarded back to the agent via its hook protocol. Yes, "Yes, allow this rule", No — all of it.
- **Window jumping** — click any session to focus its window (Hyprland, Niri).
- **Sound alerts** — configurable audio cues for approval requests, task completion, errors.
- **Catppuccin theming** — Mocha in dark mode, Latte in light mode, following the system `color-scheme` automatically.
- **Zero daemon bloat** — a single Rust binary with optional GUI/sound features.

## Supported environments

| Component    | Support                                                                     |
|--------------|-----------------------------------------------------------------------------|
| Compositors  | Hyprland, Niri                                                              |
| Bars         | Waybar                                                                      |
| Agents       | Claude Code (full, incl. approvals), Codex (full), Cursor / WebStorm (presence only) |

*"Full" = vibewatch receives granular hook events (tool calls, approvals, session lifecycle). "Presence" = the process is detected but no per-tool state is available.*

## Install

From source (requires a recent Rust toolchain):

```bash
cargo install --git https://github.com/Moinax/vibewatch
```

Headless install (no GUI panel, no sound):

```bash
cargo install --git https://github.com/Moinax/vibewatch --no-default-features
```

## Quick start

### 1. Run the daemon

```bash
vibewatch daemon
```

Or as a user service:

```bash
install -Dm644 contrib/vibewatch.service ~/.config/systemd/user/vibewatch.service
systemctl --user enable --now vibewatch
```

### 2. Wire it into Waybar

See [`contrib/waybar-module.jsonc`](contrib/waybar-module.jsonc) — add the snippet to your Waybar config and drop `"custom/vibewatch"` into your modules list.

```jsonc
"custom/vibewatch": {
    "exec": "vibewatch status",
    "return-type": "json",
    "interval": 2,
    "on-click": "vibewatch toggle-panel",
    "format": "{}"
}
```

### 3. Wire it into your agents

<details>
<summary><b>Claude Code</b> — add to <code>~/.claude/settings.json</code></summary>

```json
{
  "hooks": {
    "SessionStart":      [{ "matcher": "", "hooks": [{ "type": "command", "command": "vibewatch notify session-start --agent claude-code",      "async": true }] }],
    "UserPromptSubmit":  [{ "matcher": "", "hooks": [{ "type": "command", "command": "vibewatch notify user-prompt-submit --agent claude-code", "async": true }] }],
    "PreToolUse":        [{ "matcher": "", "hooks": [{ "type": "command", "command": "vibewatch notify pre-tool-use --agent claude-code",       "async": true }] }],
    "PostToolUse":       [{ "matcher": "", "hooks": [{ "type": "command", "command": "vibewatch notify post-tool-use --agent claude-code",      "async": true }] }],
    "PermissionRequest": [{ "matcher": "", "hooks": [{ "type": "command", "command": "vibewatch notify permission-request --agent claude-code" }] }],
    "Stop":              [{ "matcher": "", "hooks": [{ "type": "command", "command": "vibewatch notify stop --agent claude-code",               "async": true }] }]
  }
}
```

The `PermissionRequest` hook **must run synchronously** (no `async: true`) — that's how vibewatch forwards your click back to Claude Code as the permission decision.
</details>

<details>
<summary><b>Codex</b> — add to <code>~/.codex/hooks.json</code></summary>

```json
{
  "hooks": {
    "SessionStart": [{ "matcher": "", "hooks": [{ "type": "command", "command": "vibewatch notify session-start --agent codex" }] }],
    "PreToolUse":   [{ "matcher": "", "hooks": [{ "type": "command", "command": "vibewatch notify pre-tool-use --agent codex" }] }],
    "PostToolUse":  [{ "matcher": "", "hooks": [{ "type": "command", "command": "vibewatch notify post-tool-use --agent codex" }] }],
    "Stop":         [{ "matcher": "", "hooks": [{ "type": "command", "command": "vibewatch notify stop --agent codex" }] }]
  }
}
```
</details>

## Configuration

vibewatch reads `~/.config/vibewatch/config.toml` if present. All fields are optional.

```toml
[general]
compositor = "auto"          # "auto", "hyprland", or "niri"
# socket_path = "/run/user/1000/vibewatch.sock"

[sounds]
enabled = true
approval_needed = "builtin:chime"     # or a path to a .wav
task_complete   = "builtin:success"
error           = "builtin:alert"

[agents.cursor]
window_class = "cursor"

[agents.webstorm]
window_class = "jetbrains-webstorm"
```

## CLI

| Command                             | Description                                                   |
|-------------------------------------|---------------------------------------------------------------|
| `vibewatch daemon`                  | Start the daemon (auto-embeds the GTK panel when `WAYLAND_DISPLAY` is set) |
| `vibewatch status`                  | Emit the current session snapshot as JSON (for Waybar)        |
| `vibewatch toggle-panel`            | Show/hide the overlay panel                                   |
| `vibewatch notify <event> --agent <name>` | Forward a hook event (reads the payload from stdin)     |

## Contributing

vibewatch is early and opinionated — but contributions, ideas, and issue reports are welcome. Want support for a new compositor, bar, or agent? Open an issue describing the events the agent emits and we'll see what fits.

## License

MIT — see [LICENSE](LICENSE).
