# vibewatch

**A status bar and glanceable overlay for your AI coding agents — on Wayland.**

vibewatch is an open-source alternative to [Vibe Island](https://vibeisland.app/) built for Linux, Hyprland, and Niri. It listens to your agents' hooks in real time and gives you a single place to see what every Claude Code or Codex session is doing — and, when one stops to ask for permission, lets you **approve or deny right from the overlay** instead of hunting for the right terminal tab.

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
- **Waybar module** — compact JSON module wearing the lead agent's own logo, with click-to-open behavior.
- **GTK4 overlay panel** — layer-shell popup with a card per session: name, agent, terminal, current tool, elapsed time. Slides in like a drawer from the top edge and auto-hides once nothing needs your attention and the mouse leaves it (both configurable).
- **Ranked, bounded list** — agents waiting on you come first, then the ones that just finished, then the ones working, then idle ones most-recently-active first. The list shows `panel.max_visible` rows (and never more than a third of the screen) and scrolls for the rest, so a 15-agent fleet still fits on screen.
- **Pops open on completion** — the panel slides down along with the completion chime and lights up the agent that earned it, so the sound tells you *someone* is done and the overlay tells you *which one*. It stays open and lit until you click the card or the agent picks the work back up — a finished turn can't scroll past while you're heads-down elsewhere. `panel.open_on_finish = false` keeps the sound without the pop-up. A turn that ends only to be picked straight back up is a real stop but not a real finish, and two things keep it quiet. Sub-agents are counted: launching one via the `Agent`/`Task` tool arms the session, each sub-agent's own `Stop` disarms it, and while any are outstanding the agent is understood to be parked waiting rather than done — for however many minutes that takes, which no timeout could cover. Then, once nothing is outstanding, `general.idle_debounce_ms` covers the last second or two, because the final sub-agent reporting in is immediately followed by the main agent being re-invoked. The result is one chime per task instead of five. A sub-agent that dies without reporting can't wedge this: the hold expires after `general.hold_ceiling_ms` of complete stillness — a live sub-agent's own tool hooks land on the parent session, so anything actually running cancels that wait — and the count resets on every user prompt.
- **Click means "take me there"** — clicking any card focuses that agent's pane *and* rolls the drawer up, since the overlay sits right over the window you're heading for. On a finished agent that click is also the acknowledgement that clears its highlight; on one waiting for approval it just gets out of the way, leaving the prompt for you to answer in the pane.
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
2. Include `~/.config/vibewatch/waybar-module.jsonc` in your Waybar layout and add `"custom/vibewatch"` to your modules. Add `@import url("../vibewatch/logos.css");` at the top of your waybar stylesheet for the agent marks, and optionally import [`contrib/waybar-style.css`](contrib/waybar-style.css) too — see [Waybar styling](#waybar-styling) below.
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
idle_debounce_ms = 3000      # quiet required before a finish is announced, once
                             # nothing is outstanding; 0 announces every turn
hold_ceiling_ms = 300000     # how long a turn held open by outstanding sub-agents
                             # may stay silent before it is announced anyway, in
                             # case none of them ever reports; 0 holds forever

[sounds]
enabled = true
approval_needed = "builtin:chime"     # or a path to a .wav
error           = "builtin:alert"

[panel]
animate       = true   # slide the overlay in/out like a drawer from the top
animation_ms  = 220    # drawer slide duration
auto_close    = true   # hide once nothing needs attention (no pending approval,
                       # no unacknowledged finish) and the mouse leaves
auto_close_ms = 5000   # idle delay before auto-closing
open_on_finish = true  # pop the overlay open when an agent finishes its turn,
                       # highlighting the one that just chimed
max_visible   = 5      # rows shown before the list scrolls (height is also
                       # capped at a third of the monitor, whichever is smaller)

[agents.cursor]
window_class = "cursor"

[agents.webstorm]
window_class = "jetbrains-webstorm"
```

## Waybar styling

The widget reads as three blocks — **who**, a dim separator, **what**:

```
<mark> vibewatch │ Bash  +3        one agent leads, three others behind it
<mark> vibewatch │ ✔ done  +3      that agent just finished its turn
<mark> VibeWatch · 4 idle          nobody is working, so the brand stands in
```

Whose name is shown follows the same ranking the panel groups its list by (`activity_band`): blocked on you first, then a turn that just finished, then whatever is working. The others are a trailing `+n` badge rather than a leading count — a bare `4` in front promised four agents while showing one, and read as part of the name.

The daemon emits the line as a JSON object with these relevant fields:

| Field    | Meaning                                                                                          |
|----------|--------------------------------------------------------------------------------------------------|
| `text`   | Widget label, e.g. `dotfiles <span foreground="#6c7086">│</span> <span foreground="#74c7ec">thinking</span>` — Pango markup inline |
| `class`  | Two entries: the state (`idle`, `active`, `attention`) and the mark (`logo-*`), as an array so waybar replaces the whole list on each update |
| `sessions` | Full session snapshot (panel consumers; ignored by waybar)                                     |

The **status word** is colored by the daemon using the [Catppuccin](https://catppuccin.com/) palette (Mocha when the system is in dark mode, Latte otherwise — detected via `gsettings get org.gnome.desktop.interface color-scheme`):

| Status                  | Mocha     | Latte     |
|-------------------------|-----------|-----------|
| `thinking`              | `#74c7ec` | `#209fb5` |
| `executing` / `running` | `#a6e3a1` | `#40a02b` |
| `✔ done` (just finished)| `#fab387` | `#fe640b` |
| `waiting-approval`      | *(skipped — handled by `.attention` CSS background)* | |
| `idle` / `stopped`      | `#6c7086` | `#8c8fa1` |

So the inline color always looks "right" on any bar theme without needing user CSS. The one thing you do want to style yourself is the `.attention` state (a session is blocked on a widget click), because its visibility depends on your bar's background.

The widget uses Waybar's **continuous** custom-module mode: `vibewatch status --watch` stays connected to the daemon and writes one JSON line per transition, so the bar updates the instant a tool starts or a prompt arrives — no 2 s polling lag. The module config has no `interval` field (see `contrib/waybar-module.jsonc`).

A reference snippet lives at [`contrib/waybar-style.css`](contrib/waybar-style.css) — drop it into your waybar `style.css` or `@import` it:

```css
/* in ~/.config/waybar/style.css */
@import url("/path/to/contrib/waybar-style.css");
```

Or copy the three rules directly and tweak the peach background to match your bar's accent color — e.g. `rgba(255, 100, 255, 0.5)` for a magenta theme. The selector is `#custom-vibewatch.attention`.

### Agent marks

The widget wears the logo of whichever agent is in the lead, and vibewatch's own when the fleet is asleep. It cannot be a character in the label: no Nerd Font ships a Claude or an OpenAI glyph, and every hexagon in the usual font stacks collapses into an indistinguishable dot at 14 px. So the daemon names the agent in the class list (`logo-claude`, `logo-codex`, `logo-cursor`, `logo-webstorm`, `logo-vibewatch`) and GTK paints the matching SVG as a `background-image`.

`vibewatch install` writes the marks and the stylesheet that maps them into `~/.config/vibewatch/`. Add one line at the **top** of your waybar stylesheet — GTK only honours `@import` before any declaration:

```css
@import url("../vibewatch/logos.css");
```

Waybar 0.15 loads `style-dark.css` and `style-light.css` directly, so the import goes in both if you keep the pair. The URLs inside `logos.css` are relative to *it*, not to the sheet importing it, which is why nothing needs an absolute home path. Without the import the widget still works, just bare.

The marks are single-colour by necessity — GTK paints a `background-image` as-is and cannot recolour it per state, so the state colour stays in the text — and each is picked to hold up on a light and a dark chip alike, so one set serves both themes. The Claude and Codex files are approximations of the vendors' own marks; drop the official assets over them and nothing in the code has to change. Cursor gets a pointer and WebStorm a generic editor badge, deliberately: their real marks are unreadable at this size.

One caveat if you edit them: **librsvg parses strict XML and rejects a whole file over a double hyphen inside a comment**, leaving the widget silently mark-less. `rsvg-convert -w 17 file.svg -o /tmp/x.png` is the check that matters — Inkscape and ImageMagick are more forgiving than GTK is.

## CLI

| Command                             | Description                                                   |
|-------------------------------------|---------------------------------------------------------------|
| `vibewatch daemon`                  | Start the daemon (auto-embeds the GTK panel when `WAYLAND_DISPLAY` is set) |
| `vibewatch status`                  | Emit the current session snapshot as JSON (one-shot)          |
| `vibewatch status --watch`          | Stream JSON lines on every state change (for Waybar continuous mode) |
| `vibewatch toggle-panel`            | Show/hide the overlay panel                                   |
| `vibewatch notify <event> --agent <name>` | Forward a hook event (reads the payload from stdin)     |

## Contributing

vibewatch is early and opinionated — but contributions, ideas, and issue reports are welcome. Want support for a new compositor, bar, or agent? Open an issue describing the events the agent emits and we'll see what fits.

## License

MIT — see [LICENSE](LICENSE).
