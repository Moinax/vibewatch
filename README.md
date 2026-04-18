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

```bash
curl -fsSL https://raw.githubusercontent.com/Moinax/vibewatch/main/install.sh | sh
```

That script does three things automatically: builds the binary (via `cargo install --git`), installs the user-systemd service, and merges vibewatch's hooks into `~/.claude/settings.json`.

You'll still need to do three short steps by hand — `vibewatch install` prints copy-paste snippets for each:

1. Add `exec-once = ~/.cargo/bin/vibewatch daemon` (Hyprland) or the equivalent `spawn-at-startup` line (Niri) to your compositor config.
2. Include `~/.config/vibewatch/waybar-module.jsonc` in your Waybar layout and add `"custom/vibewatch"` to your modules.
3. (Optional) For cleanest widget-click-to-focus on Hyprland, add `cursor { no_warps = true }` and `input { mouse_refocus = false }`.

Flags: `vibewatch install --help` — `--no-service`, `--no-hooks`, `--dry-run`, `--uninstall`.

## Uninstall

```bash
vibewatch install --uninstall
cargo uninstall vibewatch
```

`--uninstall` stops & disables the service, removes the unit file, strips vibewatch hooks from `~/.claude/settings.json` (other hooks untouched), and deletes `~/.config/vibewatch/`.

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
