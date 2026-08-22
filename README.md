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
- **The card says what it's doing, then what it said** — while a tool runs, the card shows it (the command, the file); once the turn ends, it shows the agent's own closing sentence, whatever landed after it. A background task reporting in, or the tool the turn happened to end on, no longer takes that line, and a prompt Claude Code submits on your behalf never renders as something you said. Always one line: newlines are flattened, so a multi-line command can't grow the card.
- **Ranked, bounded list** — agents waiting on you come first, then the ones that just finished, then the ones working, then idle ones most-recently-active first. The list shows `panel.max_visible` rows (and never more than a third of the screen) and scrolls for the rest, so a 15-agent fleet still fits on screen.
- **Pops open on completion** — the panel slides down along with the completion chime and lights up the agent that earned it, so the sound tells you *someone* is done and the overlay tells you *which one*. The drawer itself closes on the usual `panel.auto_close_ms` dwell — parking it over the work until you notice was worse than closing — but the row stays lit until you acknowledge it or the agent picks the work back up, so a finished turn can't scroll past while you're heads-down elsewhere. `panel.open_on_finish = false` keeps the sound without the pop-up, and the eye button in the panel header turns every self-opening off for the session and back on again — waybar keeps reporting either way, and the click toggle still opens the drawer by hand. Its state, like the speaker button's, is remembered across restarts. A turn that ends only to be picked straight back up is a real stop but not a real finish, and two things keep it quiet. Sub-agents are counted: launching one via the `Agent`/`Task` tool arms the session, each sub-agent's own `Stop` disarms it, and while any are outstanding the agent is understood to be parked waiting rather than done — for however many minutes that takes, which no timeout could cover. Then, once nothing is outstanding, `general.idle_debounce_ms` covers the last second or two, because the final sub-agent reporting in is immediately followed by the main agent being re-invoked. The result is one chime per task instead of five. A sub-agent that dies without reporting can't wedge this: the hold expires after `general.hold_ceiling_ms` of complete stillness — a live sub-agent's own tool hooks land on the parent session, so anything actually running cancels that wait — and the count resets on every user prompt.
- **Click means "take me there"** — clicking any card focuses that agent's pane *and* rolls the drawer up, since the overlay sits right over the window you're heading for. On a finished agent that click is also the acknowledgement that clears its highlight; on one waiting for approval it just gets out of the way, leaving the prompt for you to answer in the pane. When you only want to say "seen" without going anywhere, a finished row carries a **Seen** bar under the card — the same shape the approval buttons take, since a finish is the only other thing a row asks of you: it clears the highlight and leaves you in the panel.
- **Account limits above the fleet** — how much of each provider's quota is spent, as a bar per rolling window: Claude's 5-hour, weekly and per-model weeklies, Codex's week. Each row carries the share used and when the window rolls over, and a provider whose figures have gone stale says how old they are rather than passing them off as current. The share turns amber past 80% and red past 95%, and a warning mark appears beside the section title so a hot window is visible even with the section folded away — click the title to fold it, a choice remembered across restarts like the speaker and eye buttons. The windows are whatever the provider reports, so one it adds or brings back appears on its own. Claude's figures come from its account endpoint, using the token Claude Code already holds: they exist nowhere on disk, since Claude streams them to a live session and persists nothing. That is the one outbound request vibewatch makes and `limits.enabled = false` turns it off, leaving the section out and the daemon entirely local. Codex needs no network either way — it writes its own snapshot beside every token count. Both are cached in `$XDG_STATE_HOME/vibewatch/account-limits.json` and re-read at most once every five minutes, on a finished turn or a panel open, so the numbers follow spending rather than a timer.
- **Click-to-approve** — Claude Code permission prompts are rendered as real buttons inside the overlay, forwarded back to the agent via its hook protocol. Yes, "Yes, allow this rule", No — all of it.
- **Window jumping** — click any session to focus its window (Hyprland, Niri).
- **Sound alerts** — configurable audio cues for approval requests, task completion, errors.
- **Catppuccin theming** — Mocha in dark mode, Latte in light mode, following the system `color-scheme` automatically.
- **Zero daemon bloat** — a single Rust binary with optional GUI, sound and account-limit features; a headless build carries no GTK and no TLS stack.

## Supported environments

| Component    | Support                                                                     |
|--------------|-----------------------------------------------------------------------------|
| Compositors  | Hyprland, Niri                                                              |
| Bars         | Waybar                                                                      |
| Agents       | Claude Code (full, incl. approvals), Codex (full), Cursor / WebStorm (presence only) |
| Hosts        | Terminals, Zellij, herdr, T3 Code                                           |

*"Full" = vibewatch receives granular hook events (tool calls, approvals, session lifecycle). "Presence" = the process is detected but no per-tool state is available.*

*"Hosts" are the places an agent can be running. Whichever it is, the card's badge names it and clicking the card takes you there.*

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
auto_close    = true   # hide once nothing needs attention (no pending approval)
                       # and the mouse leaves
auto_close_ms = 3000   # idle delay before auto-closing
open_on_finish = true  # pop the overlay open when an agent finishes its turn,
                       # highlighting the one that just chimed
max_visible   = 5      # rows shown before the list scrolls (height is also
                       # capped at a third of the monitor, whichever is smaller)

[limits]
enabled = true      # show account quota above the agent list; off also stops
                    # the one request vibewatch makes, leaving it fully local

[t3]
enabled   = true    # track the agents T3 Code runs for its threads
deep_link = true    # on click, also ask T3 Code to open the thread; set false
                    # on a machine with no T3 desktop app to claim the scheme

[agents.cursor]
window_class = "cursor"

[agents.webstorm]
window_class = "jetbrains-webstorm"
```

## T3 Code

[T3 Code](https://t3.codes) runs each of its threads as a headless `claude` or `codex` of its own, driven over stdio from the app's server process. vibewatch picks those up like any other session — same states, same chime, same waybar line — with two differences:

- The row is named after the **T3 thread**, not the agent's transcript title, and carries a `T3 Code` badge where a terminal name would be. When T3 says a thread is waiting on you, the row says which of the three it is — `awaiting approval`, `awaiting answer` or `plan ready` — read from the three counters T3's own sidebar ranks by. For these agents T3 owns the prompt rather than Claude, so its answer is the only one there is: our hooks never see the gate, and the agent's last tool is stale by the time it matters.
- Clicking the row raises the **T3 Code window** and asks T3 to select the thread inside it, over `t3code://threads/<environment>/<thread>` — the URL T3's own mobile widgets use. A T3 build that does not route that link still reveals its window, so the click lands you in the app either way, on whichever thread was last open. Upstream T3 does not route it yet ([pingdotgg/t3code#6008](https://github.com/pingdotgg/t3code/pull/6008)). Set `deep_link = false` on a machine with no T3 desktop app: nothing claims the scheme there, so the desktop would ask which application to open it with.

Thread titles and ids are read from T3's own state database, opened read-only. Without the `t3` cargo feature that read is skipped and the sessions simply wear the agent's own title; `enabled = false` drops them from the panel altogether, back to treating every headless agent as a script's.

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
| `text`   | Widget label, e.g. `dotfiles <span foreground="#6c7086">│</span> <span foreground="#74d4ff">thinking</span>` — Pango markup inline |
| `class`  | Two entries: the state (`idle`, `active`, `attention`) and the mark (`logo-*`), as an array so waybar replaces the whole list on each update |
| `sessions` | Full session snapshot (panel consumers; ignored by waybar)                                     |

The **status word** is colored by the daemon using the [Catppuccin](https://catppuccin.com/) palette (Mocha when the system is in dark mode, Latte otherwise). The daemon follows the scheme live: GTK's style manager hears the portal's changes and pushes the flavour into the bar's payload at the same moment it swaps the panel's own palette. Short-lived CLI invocations have no GTK to ask, so they seed from `gsettings get org.gnome.desktop.interface color-scheme`:

| State | Word | Icon | Mocha | Latte |
|-------|------|------|-------|-------|
| `working` | `thinking`, or the tool | `md-thought_bubble` / per tool | `#74d4ff` sky-300 | `#0084d1` |
| `waiting-approval` | `awaiting approval` | `fa-question` | `#fab387` peach | `#fe640b` |
| `awaiting-input` | `awaiting answer` | `fa-question` | `#b4befe` lavender | `#7287fd` |
| `plan-ready` | `plan ready` | `fa-question` | `#cba6f7` mauve | `#8839ef` |
| `just-finished` | `done` | `U+2714`, a heavy check | `#a6e3a1` green | `#40a02b` |
| `idle` | `idle` / `stopped` | `md-sleep`, a literal zᶻᶻ | `#6c7086` grey | `#8c8fa1` |

The assignment is deliberate and borrowed: it is [T3 Code](https://t3.codes)'s
thread-status vocabulary, which solves the same problem for the same kind of
list. **Warm means act, blue means the machine is busy, green means resolved,
grey means nothing to say.** Three consequences worth spelling out, because each
one replaced an earlier choice here:

- **Thinking and executing are one colour.** The difference between them is real
  and never the user's business, so the indicator glyph carries it — a
  distinction that costs nothing — and the hue is freed for something louder.
- **A finish is green, not peach.** Orange says *warning* about an outcome that
  is good news, and it cost the set its only calm-but-visible colour.
- **"Blocked on you" is three states, not one.** A permission gate, a question
  and a plan awaiting a verdict are three different asks, and which one it is
  decides what you reach for. `Session::state_kind` splits them from
  `current_tool`; for a T3-hosted thread, from T3's own three counters.

Only what is blocked on you pulses. A finish gets a tint in the panel and the
drawer opening, which is enough — it should be findable without competing with
a row that is actually waiting.

While a tool runs, the icon says what kind of work it is: a terminal for `Bash`, a pencil for `Edit`/`Write`, a document for `Read`, a magnifier for `Grep`/`Glob`, a globe for `WebFetch`, a robot for a sub-agent (`Agent`/`Task`), a checklist for `TodoWrite`, a plug for any MCP tool. Anything else runs *something*, so it falls back to the terminal. The table is `Session::tool_icon`.

The word comes from one place, `Session::state_label`, so the bar and the panel cannot describe the same session differently — they did once, the bar saying `✔ done` while the panel said `finished`. The shape comes from another, `Session::indicator_glyph`, so the state survives being read by someone who cannot tell peach from green.

Word and shape stay separate because the two surfaces have different room for them. The panel has an indicator column and draws the shape there, next to the bare word. The waybar is a single label with no such column, so for the one state whose word would otherwise be a bare `done` in a colour it inlines the mark: `✔ done`. Every other state names a tool or an action and carries itself.

Every icon is checked rendered at 13px, the size the panel's indicator draws at, because that is where they fail: anything with internal detail collapses into a smudge. A brain for `thinking` was tried and dropped for exactly that — it needs 17px before it reads as a brain — so thinking wears a thought bubble instead.

These are Nerd Font glyphs, so **the panel needs a Nerd Font in its font stack**, which `assets/style.css` names on `.indicator`. Without one that row draws tofu; the waybar module already required a Nerd Font for the agent marks, so this only extends the requirement to the panel.

`running` is the scan's "process is alive, nothing reported yet" state. It reads as `idle` in both surfaces — same word, same glyph, and now the same grey. It used to be green in the bar alone, so a session nothing had ever been heard from lit up as busy.

So the inline color always looks "right" on any bar theme without needing user CSS. The one thing you do want to style yourself is the `.attention` state (a session is blocked on a widget click), because its visibility depends on your bar's background.

The widget uses Waybar's **continuous** custom-module mode: `vibewatch status --watch` stays connected to the daemon and writes one JSON line per transition, so the bar updates the instant a tool starts or a prompt arrives — no 2 s polling lag. The module config has no `interval` field (see `contrib/waybar-module.jsonc`).

### Splitting the pill

Pango can colour text but cannot draw a rounded background, so a state word in its own chip needs to be a real widget. Waybar gives you that through a `group/` of children — the same mechanism `group/stats` uses for `cpu` and `memory` — and each child needs its own `exec`, since one stream cannot feed two widgets. Hence `--part`:

```jsonc
"group/vibewatch": {
  "orientation": "horizontal",
  "modules": ["custom/vibewatch-name", "custom/vibewatch-state", "custom/vibewatch-count"]
}
```

with each child running `vibewatch status --watch --part name` / `state` / `count`. The full snippet, and the CSS to go with it, are in [`contrib/waybar-module.jsonc`](contrib/waybar-module.jsonc). Use `custom/vibewatch` instead if you want the single-label form; it is still the default and unchanged.

Three things are easy to get wrong here:

- **A `group/` does not match `.module`.** It is a box, not a module, so the pill background has to name `#vibewatch` explicitly or each child ends up carrying its own chip and the bar grows extra pills.
- **A group emits no CSS class.** Only the children do, so the group cannot know the fleet is blocked on you; put `.attention` on a child. vibewatch's own bar puts it on the state chip, which reads better anyway — the state is the thing waiting.
- **Do not reach for CSS `:empty`** to collapse the count child when one agent is alive. GTK has no such pseudo-class, and it does not degrade gracefully: waybar rejects the whole stylesheet and exits, taking the bar with it. Waybar's own `"hide-empty-text": true` drops the widget instead, padding and all. (The daemon also puts an `empty` class on that part, for anyone who would rather style it than hide it.)

The recess is translucent black rather than a solid colour on purpose. The bar is see-through, so the chip has to darken the wallpaper showing through it too; an opaque fill would ignore whatever is behind and read as a floating slab.

Cost, measured rather than assumed: each subscriber is ~5 MB PSS, so a three-child group across three monitors is ~46 MB against ~15 MB for the single module. (Its ~28 MB RSS is mostly shared library pages, counted once per process — RSS overstates this by 5×.)

A reference snippet lives at [`contrib/waybar-style.css`](contrib/waybar-style.css) — drop it into your waybar `style.css` or `@import` it:

```css
/* in ~/.config/waybar/style.css */
@import url("/path/to/contrib/waybar-style.css");
```

Or copy the rules directly and tweak the `.attention` rim to match your bar's accent color — e.g. `rgba(255, 100, 255, 0.85)` for a magenta theme. The selector is `#custom-vibewatch.attention`. Keep it a rim rather than a fill: the state word arrives already coloured, and a tinted ground swallows the half of the vocabulary that sits near its hue.

### Agent marks

The widget wears the logo of whichever agent is in the lead, and vibewatch's own when the fleet is asleep. It cannot be a character in the label: no Nerd Font ships a Claude or an OpenAI glyph, and every hexagon in the usual font stacks collapses into an indistinguishable dot at 14 px. So the daemon names the agent in the class list (`logo-claude`, `logo-codex`, `logo-cursor`, `logo-webstorm`, `logo-vibewatch`) and GTK paints the matching SVG as a `background-image`.

`vibewatch install` writes the marks and the stylesheet that maps them into `~/.config/vibewatch/`. Add one line at the **top** of your waybar stylesheet — GTK only honours `@import` before any declaration:

```css
@import url("../vibewatch/logos.css");
```

Waybar 0.15 loads `style-dark.css` and `style-light.css` directly, so the import goes in both if you keep the pair. The URLs inside `logos.css` are relative to *it*, not to the sheet importing it, which is why nothing needs an absolute home path. Without the import the widget still works, just bare.

The marks are single-colour by necessity — GTK paints a `background-image` as-is and cannot recolour it per state, so the state colour stays in the text — and each is picked to hold up on a light and a dark chip alike, so one set serves both themes. The Claude and Codex files are approximations of the vendors' own marks; drop the official assets over them and nothing in the code has to change. Cursor gets a pointer and WebStorm a generic editor badge, deliberately: their real marks are unreadable at this size.

One caveat if you edit them: **librsvg parses strict XML and rejects a whole file over a double hyphen inside a comment**, leaving the widget silently mark-less. `rsvg-convert -w 17 file.svg -o /tmp/x.png` is the check that matters — Inkscape and ImageMagick are more forgiving than GTK is.

## Session names

A session is named after the agent's own title — the one Claude Code keeps for the conversation, what `/rename` writes and what it resharpens as the work drifts. It is re-read on every scan tick, so a `/rename` shows up in the panel and the bar within seconds without any hook.

If something outside vibewatch has a better name, push it in:

```sh
vibewatch rename <session-id> "auth token refresh"
```

That name outranks the agent's title, because a name a person typed beats a model's summary of the work. It is not permanent, though: the agent's title as it stood is banked at that moment, and the first time the title says something *different* the agent gets the say back. So a hand-typed name survives the scan tick that would otherwise wipe it two seconds later, without freezing the session's name forever.

The intended caller is a multiplexer hook. In this repo's own setup, [`herdr-agent-title`](https://github.com/Moinax/dotfiles) runs on Claude Code's `Stop`/`SessionStart` and names the herdr tab, the sidebar and vibewatch together — so renaming a tab by hand renames it everywhere instead of leaving the bar on the old title.

The name is held in memory, so a daemon restart drops it back to the agent's title. A caller that wants it to stick should re-send on every turn; the call is idempotent.

## CLI

| Command                             | Description                                                   |
|-------------------------------------|---------------------------------------------------------------|
| `vibewatch daemon`                  | Start the daemon (auto-embeds the GTK panel when `WAYLAND_DISPLAY` is set) |
| `vibewatch status`                  | Emit the current session snapshot as JSON (one-shot)          |
| `vibewatch status --watch`          | Stream JSON lines on every state change (for Waybar continuous mode) |
| `… --part name\|state\|count`        | Stream one slice instead of the whole line — one per child of a `group/` (see [Splitting the pill](#splitting-the-pill)) |
| `vibewatch toggle-panel`            | Show/hide the overlay panel                                   |
| `vibewatch rename <id> <name>`      | Name a session yourself, overriding the agent's own title (see [Session names](#session-names)) |
| `vibewatch notify <event> --agent <name>` | Forward a hook event (reads the payload from stdin)     |

## Contributing

vibewatch is early and opinionated — but contributions, ideas, and issue reports are welcome. Want support for a new compositor, bar, or agent? Open an issue describing the events the agent emits and we'll see what fits.

## License

MIT — see [LICENSE](LICENSE).
