use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use crate::ipc::{StatusPart, StatusResponse};
use crate::session::{Session, SessionStatus, StateKind};

/// The state colours the bar tints its inline Pango word with — one field per
/// [`StateKind`], mirroring the same six rules in `assets/style.css` so the
/// pill and the panel row agree on what a colour means.
///
/// Named for the *state*, not the hue, and that is the whole point: the hexes
/// are Catppuccin tokens (`palette-*.css`) but which token goes where is the
/// vocabulary, and a field called `green` invites the next state that wants
/// green to take it. Warm = act, blue = busy, green = resolved, grey = nothing.
/// The reasoning behind that assignment is on [`StateKind`].
struct Palette {
    /// T3Code's own Working blue — Tailwind sky-300 on dark, sky-600 on light
    /// (the pairing T3Code itself uses). Thinking and executing both, the way T3Code paints Working
    /// and Connecting one colour: the machine is busy, there is nothing here
    /// for you, look away. Which tool is running is on the glyph beside it.
    working: &'static str,
    /// Peach — the loudest hue in the set, and the furthest from `working`,
    /// because a permission gate is the one state that stops all progress
    /// until you look. Also what `contrib/waybar-style.css` has always
    /// suggested for the `.attention` chip.
    approval: &'static str,
    /// Lavender. Blocked on you like `approval`, but on an answer rather than
    /// a yes/no — a different ask, so a different colour, off the warm end
    /// since nothing is gated on a permission you might refuse.
    input: &'static str,
    /// Mauve. A plan wants a verdict — an ask that is neither a gate nor a
    /// question, and the one T3Code found worth its own hue too.
    plan: &'static str,
    /// Green. The turn is over and it went fine; the traffic-light reading of
    /// green is *resolved*, which is the one thing a finish is. This used to be
    /// peach, which said "warning" about an outcome that is good news, and it
    /// cost the vocabulary its only calm-but-visible colour.
    done: &'static str,
    /// Grey. Asleep, stopped, or seen by the scan and never heard from — the
    /// states T3Code gives no pill at all.
    dim: &'static str,
    /// The `+n` badge. Full-strength ink, not `dim`: the badge is the only place
    /// the fleet size appears, and at `dim` it read as decoration next to the
    /// name rather than as a number worth counting.
    badge: &'static str,
}

const MOCHA: Palette = Palette {
    working: "#74d4ff",  // sky-300
    approval: "#fab387", // peach
    input: "#b4befe",    // lavender
    plan: "#cba6f7",     // mauve
    done: "#a6e3a1",     // green
    dim: "#6c7086",
    badge: "#cdd6f4",
};

const LATTE: Palette = Palette {
    working: "#0084d1",  // sky-600
    approval: "#fe640b", // peach
    input: "#7287fd",    // lavender
    plan: "#8839ef",     // mauve
    done: "#40a02b",     // green
    dim: "#8c8fa1",
    badge: "#4c4f69",
};

/// The brand class, worn whenever no agent is in the lead — nothing running, or
/// a fleet that is entirely asleep. Its mark is vibewatch's own, so the widget
/// still says what it is at rest rather than going anonymous.
const LOGO_BRAND: &str = "logo-vibewatch";
/// Shown next to the brand mark at rest, in place of a session name that would
/// carry no signal (none of them are doing anything).
const BRAND_NAME: &str = "VibeWatch";
/// Divides *who* from *what*: the session name from the state word. A box-drawing
/// light vertical (`\u{2502}`), dimmed — the two halves used to run together as
/// one unpunctuated phrase.
const NAME_SEP: &str = "\u{2502}";

/// Which flavour the inline Pango spans wear.
///
/// Seeded from `gsettings`, which is all the short-lived CLI paths (`status`,
/// `notify`) can consult, then handed over to the daemon's GTK style manager —
/// see `set_dark_mode`. It used to be a `OnceLock<bool>` decided at boot, and
/// nothing restarts vibewatch on a theme switch (`apply-dark-mode.sh` restarts
/// swayosd and leaves this alone), so switching to Latte left the bar wearing
/// Mocha pastels: a `#a6e3a1` green on a `#eff1f5` ground, washed out to
/// near-invisible, until the daemon happened to be restarted by hand.
static DARK: OnceLock<AtomicBool> = OnceLock::new();

fn dark_flag() -> &'static AtomicBool {
    DARK.get_or_init(|| AtomicBool::new(detect_dark_mode()))
}

/// Follow the desktop's colour scheme. Called by the panel's style-manager
/// listener, which hears the portal's changes live and is authoritative — GTK
/// resolves the scheme the same way the rest of the desktop does.
///
/// A store per *switch*, deliberately: re-running `detect_dark_mode` inside
/// `active_palette` would fork `gsettings` once per emission, and an emission is
/// per subscriber per update across a ~15-pane desktop.
///
/// Seeds the flag rather than storing through `dark_flag`, which would fork
/// `gsettings` for a first value this call already knows better than — and the
/// panel's first call happens during window construction, on the GTK main
/// thread. In the daemon the subprocess is never spawned at all.
pub fn set_dark_mode(dark: bool) {
    if DARK.set(AtomicBool::new(dark)).is_err() {
        dark_flag().store(dark, Ordering::Relaxed);
    }
}

fn active_palette() -> &'static Palette {
    if dark_flag().load(Ordering::Relaxed) {
        &MOCHA
    } else {
        &LATTE
    }
}

fn detect_dark_mode() -> bool {
    std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| !s.contains("prefer-light"))
        .unwrap_or(true)
}

/// One arm per [`StateKind`], exhaustively — the bar has no opinion of its own
/// about what a session is, it only knows which ink each state gets.
fn color_for_state(kind: StateKind, palette: &Palette) -> &'static str {
    match kind {
        StateKind::Working => palette.working,
        StateKind::PendingApproval => palette.approval,
        StateKind::AwaitingInput => palette.input,
        StateKind::PlanReady => palette.plan,
        StateKind::Done => palette.done,
        StateKind::Resting => palette.dim,
    }
}

/// Cap the session name shown in the waybar. Long names (e.g. deep cwd paths)
/// would push the bar wider/taller and cause the whole bar to relayout each
/// time the active session switched, producing a visible blink.
const MAX_NAME_CHARS: usize = 24;

/// Truncate at a char boundary and append a one-char ellipsis when it grew
/// past the limit. Counted in chars (not bytes) so multibyte names aren't
/// chopped mid-codepoint.
fn truncate_name(s: &str) -> String {
    let count = s.chars().count();
    if count <= MAX_NAME_CHARS {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX_NAME_CHARS - 1).collect();
    out.push('\u{2026}');
    out
}

/// Escape Pango-reserved characters in untrusted strings so waybar doesn't
/// blank the widget when a tool name or detail contains `& < > " '`.
fn pango_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

pub fn build_status(sessions: &[Session]) -> StatusResponse {
    build_status_with_palette(sessions, active_palette())
}

/// Wrap `raw` in a Pango color span. `raw` is escaped; `color` is ours.
fn tint(color: &str, raw: &str) -> String {
    format!(
        "<span foreground=\"{}\">{}</span>",
        color,
        pango_escape(raw)
    )
}

/// The state word and its color for the session in the lead. The word comes
/// from `Session::state_label`, shared with the panel row so the two cannot
/// drift; only the colour is decided here, since the bar tints inline with
/// Pango where the panel goes through CSS.
///
/// Every state carries its shape, the same one the panel's indicator column
/// draws. The bar has no such column, so the shape goes inline ahead of the
/// word — which is the point of having shapes at all: the state should not be
/// readable by hue alone, and the bar was the half of the UI where it still was.
///
/// Both halves come from the session: `state_label` for the word,
/// `state_kind` for the ink. Neither is decided here, which is what keeps the
/// bar from saying `awaiting answer` in the colour the panel reserves for a
/// permission gate.
fn headline_status(session: &Session, palette: &Palette) -> String {
    tint(
        color_for_state(session.state_kind(), palette),
        &format!("{} {}", session.indicator_glyph(), session.state_label()),
    )
}

fn build_status_with_palette(sessions: &[Session], palette: &Palette) -> StatusResponse {
    let active: Vec<&Session> = sessions
        .iter()
        .filter(|s| s.status != SessionStatus::Stopped)
        .collect();

    let count = active.len();

    // `attention` is any of the three blocked states, not just the permission
    // gate: they differ in what they ask for, never in whether they are waiting
    // on you, and the chip is the one place that distinction does not fit.
    let class = if sessions.iter().any(|s| s.state_kind().needs_user()) {
        "attention".to_string()
    } else if sessions
        .iter()
        .any(|s| s.state_kind() == StateKind::Working)
    {
        "active".to_string()
    } else {
        "idle".to_string()
    };

    // Nothing to report: no sessions at all, or a fleet where every one of them
    // is asleep and none has an unacknowledged finish. Either way no single
    // session's name carries signal, so the widget wears the brand instead.
    let at_rest = active.iter().all(|s| {
        matches!(s.status, SessionStatus::Idle | SessionStatus::Running) && !s.just_finished()
    });
    if at_rest {
        // Untinted, so it inherits the bar's own text colour and reads like the
        // clock or the volume rather than like something switched off. Dim was
        // the wrong signal: nothing is disabled at rest, the fleet is asleep —
        // and that is what the state word beside it already says.
        let name = pango_escape(BRAND_NAME);
        // The tally is the state here, not a badge: "how many are asleep" is the
        // only thing there is to say, so it belongs where a state word goes.
        // Shape here too, so the recess never holds a bare number. Taken from a
        // session rather than hardcoded: they are all idle or scan-only by
        // definition of `at_rest`, and both wear the same ring.
        let state = active.first().map_or(String::new(), |s| {
            tint(
                palette.dim,
                &format!("{} {} idle", s.indicator_glyph(), count),
            )
        });
        let text = if count == 0 {
            name.clone()
        } else {
            format!("{} {}", name, state)
        };
        return StatusResponse {
            text,
            class,
            logo: LOGO_BRAND.to_string(),
            name,
            state,
            count: String::new(),
        };
    }

    // Whose name to show. `activity_band` is the panel's own ranking — blocked
    // on the user, then just finished, then working — so the bar and the list
    // agree on what matters most instead of each having its own opinion.
    // `interest_priority` only breaks ties inside a band (executing over
    // thinking).
    let lead = active
        .iter()
        .min_by_key(|s| (s.activity_band(), std::cmp::Reverse(s.interest_priority())))
        .expect("a non-empty fleet that is not at rest has a leader");

    let name = pango_escape(&truncate_name(&lead.display_name()));
    let state = headline_status(lead, palette);
    // The others, as a badge rather than a bare leading count: the old `4` sat
    // in front promising four agents while showing one, reading as part of the
    // name. Bright rather than dim — it was so faint it read as decoration.
    let count_badge = if count > 1 {
        tint(palette.badge, &format!("+{}", count - 1))
    } else {
        String::new()
    };

    // The single-label form. A `group/` layout ignores this and takes the three
    // parts instead, where the separation comes from the state child's own chip
    // rather than from a character.
    let mut text = format!("{} {} {}", name, tint(palette.dim, NAME_SEP), state);
    if !count_badge.is_empty() {
        text.push_str("  ");
        text.push_str(&count_badge);
    }

    StatusResponse {
        text,
        class,
        logo: lead.agent.logo_class().to_string(),
        name,
        state,
        count: count_badge,
    }
}

/// Pick the slice a subscriber asked for. The logo class rides with the name,
/// which is the child that has the room for a mark; every part keeps the state
/// class so a stylesheet can react to `attention` on any of them.
fn part_of(status: &StatusResponse, part: StatusPart) -> (&str, Vec<&str>) {
    match part {
        StatusPart::All => (
            status.text.as_str(),
            vec![status.class.as_str(), status.logo.as_str()],
        ),
        StatusPart::Name => (
            status.name.as_str(),
            vec![status.class.as_str(), status.logo.as_str()],
        ),
        StatusPart::State => (status.state.as_str(), vec![status.class.as_str()]),
        StatusPart::Count => {
            let mut classes = vec![status.class.as_str()];
            // GTK CSS has no `:empty`, and using it is not a no-op — it fails to
            // parse and waybar exits, taking the whole bar down. So the daemon
            // says so out loud and the stylesheet can collapse the child's
            // padding rather than leaving dead space where the badge isn't.
            if status.count.is_empty() {
                classes.push("empty");
            }
            (status.count.as_str(), classes)
        }
    }
}

/// Shape a `StatusResponse` as the JSON payload waybar consumes. The classes go
/// out as a whole array so waybar replaces the widget's class list each update
/// instead of accumulating stale ones — which is also why the logo has to ride
/// along in the same array rather than being set once and left alone.
pub fn payload_part(status: &StatusResponse, part: StatusPart) -> String {
    let (text, classes) = part_of(status, part);
    serde_json::json!({ "text": text, "class": classes }).to_string()
}

/// Print Waybar JSON to stdout for one slice of the line.
pub fn print_waybar_part(sessions: &[Session], part: StatusPart) {
    println!("{}", payload_part(&build_status(sessions), part));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{AgentKind, SessionStatus, ICON_DONE};

    /// Build a session with a pinned `session_name` so assertions don't
    /// depend on `/proc/<pid>/cwd` resolution inside `display_name()`.
    fn make_named(name: &str, agent: AgentKind, status: SessionStatus) -> Session {
        let mut s = Session::new(format!("{}-id", name), agent, 1000);
        s.status = status;
        s.session_name = Some(name.to_string());
        s
    }

    fn dark(sessions: &[Session]) -> StatusResponse {
        build_status_with_palette(sessions, &MOCHA)
    }

    fn light(sessions: &[Session]) -> StatusResponse {
        build_status_with_palette(sessions, &LATTE)
    }

    /// The dimmed separator, spelled once so a layout change is a one-line fix.
    const SEP_DARK: &str = "<span foreground=\"#6c7086\">\u{2502}</span>";

    #[test]
    fn nothing_running_wears_the_brand() {
        let status = dark(&[]);
        assert_eq!(status.text, "VibeWatch");
        assert_eq!(status.class, "idle");
        assert_eq!(status.logo, "logo-vibewatch");
    }

    #[test]
    fn test_thinking_uses_the_working_blue_dark() {
        let sessions = vec![make_named(
            "dotfiles",
            AgentKind::ClaudeCode,
            SessionStatus::Thinking,
        )];
        let status = dark(&sessions);
        assert_eq!(
            status.text,
            format!(
                "dotfiles {} <span foreground=\"#74d4ff\">\u{f07f6} thinking</span>",
                SEP_DARK
            )
        );
        assert_eq!(status.class, "active");
        assert_eq!(status.logo, "logo-claude");
    }

    #[test]
    fn test_thinking_uses_the_working_blue_light() {
        let sessions = vec![make_named(
            "dotfiles",
            AgentKind::ClaudeCode,
            SessionStatus::Thinking,
        )];
        let status = light(&sessions);
        assert_eq!(
            status.text,
            "dotfiles <span foreground=\"#8c8fa1\">\u{2502}</span> \
             <span foreground=\"#0084d1\">\u{f07f6} thinking</span>"
        );
    }

    #[test]
    fn test_executing_wins_over_thinking_in_multi() {
        // Same band, so interest_priority breaks the tie: the executing
        // session leads, and the others become a trailing badge.
        let sessions = vec![
            make_named("dotfiles", AgentKind::ClaudeCode, SessionStatus::Thinking),
            make_named("vibewatch", AgentKind::Codex, SessionStatus::Executing),
        ];
        let status = dark(&sessions);
        assert_eq!(
            status.text,
            format!(
                "vibewatch {} <span foreground=\"#74d4ff\">\u{f120} exec</span>  \
                 <span foreground=\"#cdd6f4\">+1</span>",
                SEP_DARK
            )
        );
        assert_eq!(status.class, "active");
        // The lead's agent, not the first session's.
        assert_eq!(status.logo, "logo-codex");
    }

    #[test]
    fn a_finished_turn_leads_over_one_still_working() {
        // The chime just fired for `vibewatch`; that is the row the eye wants,
        // even though an executing session outranks an idle one on status
        // alone. `activity_band` is what puts it first.
        let mut finished = make_named("vibewatch", AgentKind::ClaudeCode, SessionStatus::Idle);
        finished.mark_finished();
        let sessions = vec![
            make_named("dotfiles", AgentKind::Codex, SessionStatus::Executing),
            finished,
        ];
        let status = dark(&sessions);
        assert_eq!(
            status.text,
            format!(
                "vibewatch {} <span foreground=\"#a6e3a1\">{} done</span>  \
                 <span foreground=\"#cdd6f4\">+1</span>",
                SEP_DARK, ICON_DONE
            )
        );
        assert_eq!(status.logo, "logo-claude");
    }

    #[test]
    fn the_bar_says_exactly_what_the_panel_row_says() {
        // The drift this guards against, which shipped once: the bar read
        // `✔ done` while the panel read `finished`, for the same session at the
        // same moment. Both go through `Session::state_label` now, and this
        // asserts the bar shows that string rather than one of its own.
        let mut finished = make_named("vibewatch", AgentKind::ClaudeCode, SessionStatus::Idle);
        finished.mark_finished();
        for s in [
            finished,
            make_named("a", AgentKind::ClaudeCode, SessionStatus::Thinking),
            make_named("b", AgentKind::Codex, SessionStatus::WaitingApproval),
        ] {
            let word = s.state_label();
            let text = dark(&[s]).text;
            assert!(
                text.contains(&pango_escape(&word)),
                "bar text {text:?} does not carry the panel's word {word:?}"
            );
        }
    }

    #[test]
    fn the_parts_reassemble_into_the_single_label_form() {
        // A `group/` layout draws the parts in separate widgets, so nothing may
        // live only in `text`: every piece of it has to be reachable as a part,
        // or a child would silently go blank.
        let sessions = vec![
            make_named("vibewatch", AgentKind::ClaudeCode, SessionStatus::Executing),
            make_named("dotfiles", AgentKind::Codex, SessionStatus::Idle),
        ];
        let s = dark(&sessions);
        assert_eq!(s.name, "vibewatch");
        assert_eq!(s.state, "<span foreground=\"#74d4ff\">\u{f120} exec</span>");
        assert_eq!(s.count, "<span foreground=\"#cdd6f4\">+1</span>");
        assert!(!payload_part(&s, StatusPart::Count).contains("empty"));
        for piece in [&s.name, &s.state, &s.count] {
            assert!(
                s.text.contains(piece.as_str()),
                "{piece} missing from {:?}",
                s.text
            );
        }
    }

    #[test]
    fn a_lone_session_leaves_the_count_widget_empty() {
        // Not `+0`: an empty text is how a waybar child draws nothing at all.
        let s = dark(&[make_named(
            "solo",
            AgentKind::ClaudeCode,
            SessionStatus::Thinking,
        )]);
        assert!(s.count.is_empty());
        // The `empty` class is how a stylesheet collapses the child: GTK has no
        // `:empty`, and reaching for it takes the whole bar down.
        assert_eq!(
            payload_part(&s, StatusPart::Count),
            r#"{"class":["active","empty"],"text":""}"#
        );
    }

    #[test]
    fn only_the_name_part_carries_the_logo_class() {
        // The mark is a `background-image` behind a left padding, so it belongs
        // to the one child that reserves room for it.
        let s = dark(&[make_named(
            "solo",
            AgentKind::Codex,
            SessionStatus::Thinking,
        )]);
        assert!(payload_part(&s, StatusPart::Name).contains("logo-codex"));
        assert!(payload_part(&s, StatusPart::All).contains("logo-codex"));
        assert!(!payload_part(&s, StatusPart::State).contains("logo-"));
        assert!(!payload_part(&s, StatusPart::Count).contains("logo-"));
    }

    #[test]
    fn a_finished_turn_is_not_at_rest() {
        // One idle session with an unacknowledged finish must not collapse to
        // the brand: that is exactly the finish the bar used to swallow.
        let mut finished = make_named("vibewatch", AgentKind::ClaudeCode, SessionStatus::Idle);
        finished.mark_finished();
        let status = dark(&[finished]);
        assert!(
            status.text.starts_with("vibewatch "),
            "expected the session named, got {:?}",
            status.text
        );
        assert!(status.text.contains(&format!("{ICON_DONE} done")));
    }

    #[test]
    fn test_attention_class_when_waiting_approval() {
        let sessions = vec![make_named(
            "dotfiles",
            AgentKind::ClaudeCode,
            SessionStatus::WaitingApproval,
        )];
        let status = dark(&sessions);
        assert_eq!(status.class, "attention");
        assert_eq!(
            status.text,
            format!(
                "dotfiles {} <span foreground=\"#fab387\">\u{f128} awaiting approval</span>",
                SEP_DARK
            )
        );
    }

    /// The three blocked states are one status and three asks, told apart by
    /// `current_tool`. Each gets its own word *and* its own ink, and the pair
    /// has to stay in step — a question tinted like a permission gate is worse
    /// than no colour at all, because it reads as one.
    #[test]
    fn the_three_asks_each_get_their_own_word_and_ink() {
        // Through `Ask::from_tool`, the way every producer of the status
        // reaches its ask — the bar is fed the answer, it does not derive one.
        for (tool, word, ink) in [
            ("Bash", "awaiting approval", MOCHA.approval),
            ("AskUserQuestion", "awaiting answer", MOCHA.input),
            ("ExitPlanMode", "plan ready", MOCHA.plan),
        ] {
            let mut session = make_named(
                "dotfiles",
                AgentKind::ClaudeCode,
                SessionStatus::WaitingApproval,
            );
            session.blocked_on = Some(crate::session::Ask::from_tool(tool));
            // Deliberately disagreeing with the ask: a stale `current_tool` is
            // the norm by the time a gate is answered, and nothing may read it.
            session.current_tool = Some("Read".to_string());
            let status = dark(&[session]);
            assert!(
                status
                    .text
                    .contains(&format!("<span foreground=\"{ink}\">")),
                "{tool:?} should be tinted {ink}, got {:?}",
                status.text
            );
            assert!(
                status.text.contains(word),
                "{tool:?} should read {word:?}, got {:?}",
                status.text
            );
            // All three are blocked on the user, whatever they are asking for.
            assert_eq!(status.class, "attention", "{tool:?} must raise attention");
        }
    }

    /// `Running` is the scanner's "alive, nothing reported yet". It says `idle`,
    /// wears the idle glyph and bands with the idle ones — and used to be the
    /// one surface that painted it green anyway, so a session nothing had ever
    /// been heard from lit the bar up as if it were working.
    #[test]
    fn a_scan_only_session_is_not_painted_as_busy() {
        let sessions = vec![
            make_named("dotfiles", AgentKind::ClaudeCode, SessionStatus::Running),
            make_named("vibewatch", AgentKind::Codex, SessionStatus::Idle),
        ];
        let status = dark(&sessions);
        assert_eq!(status.class, "idle");
        assert!(
            !status.text.contains(MOCHA.working),
            "a scan-only fleet must not wear the working ink: {:?}",
            status.text
        );
    }

    /// The bar's hexes are the panel's tokens, spelled out.
    ///
    /// They have to be: the bar tints inline with Pango, which cannot see a GTK
    /// `@define-color`, so the same six colours exist twice — once here, once in
    /// `assets/palette-*.css` — and nothing but this test can notice when an
    /// edit to one leaves the other behind. The failure it guards is silent and
    /// wrong in the worst way: a panel row and the bar pill describing the same
    /// session in two different colours.
    #[test]
    fn the_bar_and_the_panel_are_painted_from_the_same_tokens() {
        const MOCHA_CSS: &str = include_str!("../assets/palette-mocha.css");
        const LATTE_CSS: &str = include_str!("../assets/palette-latte.css");

        /// The hex `@define-color <token>` binds in `css`.
        fn token(css: &str, name: &str) -> String {
            let needle = format!("@define-color {name} ");
            let line = css
                .lines()
                .find(|l| l.starts_with(&needle))
                .unwrap_or_else(|| panic!("no @define-color {name} in the palette"));
            line.split('#')
                .nth(1)
                .map(|hex| format!("#{}", hex.trim().trim_end_matches(';')))
                .expect("a @define-color with no hex")
        }

        for (flavour, css, palette) in [("mocha", MOCHA_CSS, &MOCHA), ("latte", LATTE_CSS, &LATTE)]
        {
            for (field, ink, name) in [
                ("working", palette.working, "cat_working"),
                ("approval", palette.approval, "cat_peach"),
                ("input", palette.input, "cat_lavender"),
                ("plan", palette.plan, "cat_mauve"),
                ("done", palette.done, "cat_green"),
                ("dim", palette.dim, "cat_text_time"),
                ("badge", palette.badge, "cat_text"),
            ] {
                assert_eq!(
                    ink,
                    token(css, name),
                    "{flavour}: Palette::{field} has drifted from {name}"
                );
            }
        }
    }

    /// Six states, six colours, in both flavours: a vocabulary where two states
    /// share ink is a vocabulary with five words in it. Distinctness is the one
    /// property the palette has to hold on its own — everything else about it is
    /// taste.
    #[test]
    fn every_state_gets_ink_of_its_own() {
        for (flavour, palette) in [("mocha", &MOCHA), ("latte", &LATTE)] {
            let inks = [
                StateKind::Working,
                StateKind::PendingApproval,
                StateKind::AwaitingInput,
                StateKind::PlanReady,
                StateKind::Done,
                StateKind::Resting,
            ]
            .map(|kind| color_for_state(kind, palette));
            let mut seen = inks.to_vec();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), inks.len(), "{flavour} reuses a state colour");
        }
    }

    #[test]
    fn test_stopped_sessions_excluded_from_count() {
        let sessions = vec![
            make_named("dotfiles", AgentKind::ClaudeCode, SessionStatus::Thinking),
            make_named("vibewatch", AgentKind::Codex, SessionStatus::Stopped),
        ];
        let status = dark(&sessions);
        // One live session, so no `+n` badge.
        assert_eq!(
            status.text,
            format!(
                "dotfiles {} <span foreground=\"#74d4ff\">\u{f07f6} thinking</span>",
                SEP_DARK
            )
        );
    }

    #[test]
    fn test_idle_single_swaps_name_for_brand() {
        let sessions = vec![make_named(
            "dotfiles",
            AgentKind::ClaudeCode,
            SessionStatus::Idle,
        )];
        let status = dark(&sessions);
        assert_eq!(status.class, "idle");
        assert_eq!(
            status.text,
            "VibeWatch <span foreground=\"#6c7086\">\u{f04b2} 1 idle</span>"
        );
        assert_eq!(status.logo, "logo-vibewatch");
    }

    #[test]
    fn test_idle_multi_swaps_name_for_brand() {
        let sessions = vec![
            make_named("dotfiles", AgentKind::ClaudeCode, SessionStatus::Idle),
            make_named("vibewatch", AgentKind::Codex, SessionStatus::Idle),
        ];
        let status = dark(&sessions);
        assert_eq!(status.class, "idle");
        assert_eq!(
            status.text,
            "VibeWatch <span foreground=\"#6c7086\">\u{f04b2} 2 idle</span>"
        );
        assert_eq!(status.logo, "logo-vibewatch");
    }

    #[test]
    fn a_scanned_but_silent_session_still_counts_as_at_rest() {
        // `Running` is the scanner's "alive, no hook data yet" state, and it
        // reads as idle everywhere else in the UI.
        let sessions = vec![make_named(
            "jacket",
            AgentKind::Codex,
            SessionStatus::Running,
        )];
        let status = dark(&sessions);
        assert_eq!(
            status.text,
            "VibeWatch <span foreground=\"#6c7086\">\u{f04b2} 1 idle</span>"
        );
    }

    #[test]
    fn payload_carries_the_state_and_the_logo_in_one_class_list() {
        let sessions = vec![make_named(
            "dotfiles",
            AgentKind::ClaudeCode,
            SessionStatus::Executing,
        )];
        let json = payload_part(&dark(&sessions), StatusPart::All);
        assert!(
            json.contains("\"class\":[\"active\",\"logo-claude\"]"),
            "both classes must ride in the same array: {json}"
        );
    }

    #[test]
    fn every_agent_kind_has_a_distinct_mark() {
        let kinds = [
            AgentKind::ClaudeCode,
            AgentKind::Codex,
            AgentKind::Cursor,
            AgentKind::WebStorm,
        ];
        let classes: std::collections::HashSet<_> = kinds.iter().map(|k| k.logo_class()).collect();
        assert_eq!(classes.len(), kinds.len(), "two agents share a logo class");
        assert!(
            !classes.contains(&LOGO_BRAND),
            "no agent may claim the brand mark"
        );
    }

    #[test]
    fn test_long_session_name_is_truncated_with_ellipsis() {
        let long = "a".repeat(40);
        let session = make_named(&long, AgentKind::ClaudeCode, SessionStatus::Thinking);
        let status = dark(&[session]);
        let expected_name = format!("{}\u{2026}", "a".repeat(MAX_NAME_CHARS - 1));
        assert!(
            status.text.contains(&expected_name),
            "expected truncated name in {:?}",
            status.text
        );
        assert!(!status.text.contains(&"a".repeat(MAX_NAME_CHARS + 1)));
    }

    #[test]
    fn test_short_session_name_is_not_truncated() {
        let session = make_named("dotfiles", AgentKind::ClaudeCode, SessionStatus::Thinking);
        let status = dark(&[session]);
        assert!(status.text.contains("dotfiles"));
        assert!(!status.text.contains('\u{2026}'));
    }

    #[test]
    fn test_truncate_respects_char_boundaries() {
        // 30 multibyte chars; each is 3 bytes in UTF-8. Naive byte slicing
        // would panic; char-based truncation must produce a valid string of
        // MAX_NAME_CHARS chars total (including the ellipsis).
        let multibyte = "é".repeat(30);
        let truncated = truncate_name(&multibyte);
        assert_eq!(truncated.chars().count(), MAX_NAME_CHARS);
        assert!(truncated.ends_with('\u{2026}'));
    }

    #[test]
    fn test_pango_escape_in_tool_name() {
        let mut session = make_named("dotfiles", AgentKind::ClaudeCode, SessionStatus::Executing);
        session.current_tool = Some("A&B<x>".to_string());
        let status = dark(&[session]);
        assert_eq!(
            status.text,
            format!(
                "dotfiles {} <span foreground=\"#74d4ff\">\u{f120} A&amp;B&lt;x&gt;</span>",
                SEP_DARK
            )
        );
    }

    #[test]
    fn the_tool_detail_never_reaches_the_bar() {
        let mut session = make_named("dotfiles", AgentKind::ClaudeCode, SessionStatus::Executing);
        session.current_tool = Some("Bash".to_string());
        session.tool_detail = Some("npm test".to_string());
        let status = dark(&[session]);
        assert_eq!(
            status.text,
            format!(
                "dotfiles {} <span foreground=\"#74d4ff\">\u{f120} Bash</span>",
                SEP_DARK
            )
        );
    }
}
