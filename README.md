# vibewatch

AI agent monitor for Wayland compositors. An open-source alternative to [Vibe Island](https://vibeisland.dev/) for Linux.

vibewatch tracks AI coding agents running on your system and surfaces their status through a Waybar module and an optional GTK4 overlay panel. It plays sound alerts when an agent needs approval or finishes a task, and lets you jump to the agent's window with a single click.

## Features

- **Live session monitoring** -- detects running agent processes and tracks their state via hook events
- **Waybar integration** -- custom JSON module showing active sessions with status icons
- **GTK4 overlay panel** -- layer-shell popup embedded in the daemon for instant show/hide toggle
- **Window jumping** -- click a session to focus its window (Hyprland and Niri)
- **Sound alerts** -- configurable audio cues for approval requests, task completion, and errors
- **Hook integration** -- receives real-time events from Claude Code and Codex hooks

## Supported Environments

| Component   | Support                                                          |
|-------------|------------------------------------------------------------------|
| Compositors | Hyprland, Niri                                                   |
| Bars        | Waybar                                                           |
| Agents      | Claude Code (full), Codex (full), Cursor (presence), WebStorm (presence) |

"Full" support means vibewatch receives hook events with tool-use details and session lifecycle. "Presence" means vibewatch detects the running process but does not receive granular events.

## Installation

### From source

```bash
cargo install --path .
```

To build without optional features (no GUI panel, no sound):

```bash
cargo install --path . --no-default-features
```

### AUR

Coming soon.

## Setup

### 1. Start the daemon

Using systemd (recommended):

```bash
cp contrib/vibewatch.service ~/.config/systemd/user/
systemctl --user enable --now vibewatch
```

Or run directly:

```bash
vibewatch daemon
```

### 2. Configure Waybar

Add the custom module to your Waybar config. See `contrib/waybar-module.jsonc` for a ready-made snippet:

```jsonc
{
    "custom/vibewatch": {
        "exec": "vibewatch status",
        "return-type": "json",
        "interval": 2,
        "on-click": "vibewatch toggle-panel",
        "format": "{}",
        "tooltip": true
    }
}
```

Then add `"custom/vibewatch"` to the modules list in your Waybar layout.

### 3. Configure Claude Code hooks

The `notify` command takes an event name as its positional argument and reads the hook JSON payload from stdin. Add the following to `~/.claude/settings.json`:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "",
        "hooks": [
          { "type": "command", "command": "vibewatch notify session-start --agent claude-code" }
        ]
      }
    ],
    "UserPromptSubmit": [
      {
        "matcher": "",
        "hooks": [
          { "type": "command", "command": "vibewatch notify user-prompt-submit --agent claude-code" }
        ]
      }
    ],
    "PreToolUse": [
      {
        "matcher": "",
        "hooks": [
          { "type": "command", "command": "vibewatch notify pre-tool-use --agent claude-code" }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "",
        "hooks": [
          { "type": "command", "command": "vibewatch notify post-tool-use --agent claude-code" }
        ]
      }
    ],
    "Stop": [
      {
        "matcher": "",
        "hooks": [
          { "type": "command", "command": "vibewatch notify stop --agent claude-code" }
        ]
      }
    ]
  }
}
```

Supported Claude Code events: `session-start`, `user-prompt-submit`, `pre-tool-use`, `post-tool-use`, `permission-request`, `permission-denied`, `stop`.

### 4. Configure Codex hooks

Add the following to `~/.codex/hooks.json`:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "",
        "hooks": [
          { "type": "command", "command": "vibewatch notify session-start --agent codex" }
        ]
      }
    ],
    "PreToolUse": [
      {
        "matcher": "",
        "hooks": [
          { "type": "command", "command": "vibewatch notify pre-tool-use --agent codex" }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "",
        "hooks": [
          { "type": "command", "command": "vibewatch notify post-tool-use --agent codex" }
        ]
      }
    ],
    "Stop": [
      {
        "matcher": "",
        "hooks": [
          { "type": "command", "command": "vibewatch notify stop --agent codex" }
        ]
      }
    ]
  }
}
```

Supported Codex events: `session-start`, `pre-tool-use`, `post-tool-use`, `stop`.

## Configuration

vibewatch reads its config from `~/.config/vibewatch/config.toml`. All fields are optional and have sensible defaults.

```toml
[general]
compositor = "auto"          # "auto", "hyprland", or "niri"
# socket_path = "/run/user/1000/vibewatch.sock"  # override IPC socket

[sounds]
enabled = true
approval_needed = "builtin:chime"     # path to .wav or "builtin:<name>"
task_complete = "builtin:success"
error = "builtin:alert"

[agents.cursor]
window_class = "cursor"

[agents.webstorm]
window_class = "jetbrains-webstorm"
```

## CLI Reference

| Command        | Description                                                        |
|----------------|--------------------------------------------------------------------|
| `daemon`       | Start the vibewatch daemon (embeds the panel; falls back to headless mode if `WAYLAND_DISPLAY` is unset) |
| `status`       | Print current session status (JSON for Waybar)                     |
| `toggle-panel` | Toggle the overlay panel visibility via IPC                        |
| `notify <event> [--agent <name>]` | Forward a hook event to the daemon (reads payload from stdin) |

### Examples

```bash
# Start the daemon in the foreground
vibewatch daemon

# Check current status
vibewatch status

# Toggle the overlay panel
vibewatch toggle-panel
```

## License

MIT -- see [LICENSE](LICENSE) for details.
