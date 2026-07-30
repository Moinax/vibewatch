use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use crate::ipc::{StatusPart, StatusResponse};
use crate::session::{Session, SessionStatus};

/// Per-status colors the waybar uses for the inline Pango-colored state word
/// (mirrors tokens from assets/palette-*.css).
struct Palette {
    green: &'static str,
    sapphire: &'static str,
    dim: &'static str,
    /// Teal accent on the attention-state pill — complementary-ish contrast
    /// with the magenta `.attention` background (set by the user's waybar
    /// CSS), and distinct from the sapphire used for `thinking`.
    attention_text: &'static str,
    /// The just-finished hue, the same peach the panel tints a finished card
    /// with (`palette-*.css`). A finish is otherwise indistinguishable from
    /// idle in the bar, which is how the chime you were away for left no trace.
    peach: &'static str,
    /// The `+n` badge. Full-strength ink, not `dim`: the badge is the only place
    /// the fleet size appears, and at `dim` it read as decoration next to the
    /// name rather than as a number worth counting.
    badge: &'static str,
}

const MOCHA: Palette = Palette {
    green: "#a6e3a1",
    sapphire: "#74c7ec",
    dim: "#6c7086",
    attention_text: "#94e2d5",
    peach: "#fab387",
    badge: "#cdd6f4",
};

const LATTE: Palette = Palette {
    green: "#40a02b",
    sapphire: "#209fb5",
    dim: "#8c8fa1",
    attention_text: "#179299",
    peach: "#fe640b",
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
fn dark_flag() -> &'static AtomicBool {
    static DARK: OnceLock<AtomicBool> = OnceLock::new();
    DARK.get_or_init(|| AtomicBool::new(detect_dark_mode()))
}

/// Follow the desktop's colour scheme. Called by the panel's style-manager
/// listener, which hears the portal's changes live and is authoritative — GTK
/// resolves the scheme the same way the rest of the desktop does.
///
/// A store per *switch*, deliberately: re-running `detect_dark_mode` inside
/// `active_palette` would fork `gsettings` once per emission, and an emission is
/// per subscriber per update across a ~15-pane desktop.
pub fn set_dark_mode(dark: bool) {
    dark_flag().store(dark, Ordering::Relaxed);
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

fn color_for_status(status: SessionStatus, palette: &Palette) -> &'static str {
    match status {
        SessionStatus::Executing | SessionStatus::Running => palette.green,
        SessionStatus::Thinking => palette.sapphire,
        SessionStatus::WaitingApproval => palette.attention_text,
        SessionStatus::Idle | SessionStatus::Stopped => palette.dim,
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
    format!("<span foreground=\"{}\">{}</span>", color, pango_escape(raw))
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
/// A just-finished turn is `Idle` as far as `status` is concerned, so the peach
/// has to be asked about separately or the finish reads as "nothing happening".
fn headline_status(session: &Session, palette: &Palette) -> String {
    let color = if session.just_finished() {
        palette.peach
    } else {
        color_for_status(session.status, palette)
    };
    tint(
        color,
        &format!("{} {}", session.indicator_glyph(), session.state_label()),
    )
}

fn build_status_with_palette(sessions: &[Session], palette: &Palette) -> StatusResponse {
    let active: Vec<&Session> = sessions
        .iter()
        .filter(|s| s.status != SessionStatus::Stopped)
        .collect();

    let count = active.len();

    let class = if sessions.iter().any(|s| s.status == SessionStatus::WaitingApproval) {
        "attention".to_string()
    } else if sessions.iter().any(|s| {
        matches!(
            s.status,
            SessionStatus::Thinking | SessionStatus::Executing | SessionStatus::Running
        )
    }) {
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
        let state = match active.first() {
            None => String::new(),
            Some(s) => tint(
                palette.dim,
                &format!("{} {} idle", s.indicator_glyph(), count),
            ),
        };
        let text = if state.is_empty() {
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
    use crate::session::{AgentKind, SessionStatus};

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
    fn test_thinking_uses_sapphire_dark() {
        let sessions = vec![make_named(
            "dotfiles",
            AgentKind::ClaudeCode,
            SessionStatus::Thinking,
        )];
        let status = dark(&sessions);
        assert_eq!(
            status.text,
            format!(
                "dotfiles {} <span foreground=\"#74c7ec\">\u{f07f6} thinking</span>",
                SEP_DARK
            )
        );
        assert_eq!(status.class, "active");
        assert_eq!(status.logo, "logo-claude");
    }

    #[test]
    fn test_thinking_uses_sapphire_light() {
        let sessions = vec![make_named(
            "dotfiles",
            AgentKind::ClaudeCode,
            SessionStatus::Thinking,
        )];
        let status = light(&sessions);
        assert_eq!(
            status.text,
            "dotfiles <span foreground=\"#8c8fa1\">\u{2502}</span> \
             <span foreground=\"#209fb5\">\u{f07f6} thinking</span>"
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
                "vibewatch {} <span foreground=\"#a6e3a1\">\u{f120} exec</span>  \
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
                "vibewatch {} <span foreground=\"#fab387\">\u{2714} done</span>  \
                 <span foreground=\"#cdd6f4\">+1</span>",
                SEP_DARK
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
        assert_eq!(s.state, "<span foreground=\"#a6e3a1\">\u{f120} exec</span>");
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
        assert!(status.text.contains("\u{2714} done"));
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
                "dotfiles {} <span foreground=\"#94e2d5\">\u{f128} awaiting approval</span>",
                SEP_DARK
            )
        );
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
                "dotfiles {} <span foreground=\"#74c7ec\">\u{f07f6} thinking</span>",
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
        let multibyte: String = std::iter::repeat('é').take(30).collect();
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
                "dotfiles {} <span foreground=\"#a6e3a1\">\u{f120} A&amp;B&lt;x&gt;</span>",
                SEP_DARK
            )
        );
    }

    #[test]
    fn test_executing_tool_detail_green_span() {
        let mut session = make_named("dotfiles", AgentKind::ClaudeCode, SessionStatus::Executing);
        session.current_tool = Some("Bash".to_string());
        session.tool_detail = Some("npm test".to_string());
        let status = dark(&[session]);
        assert_eq!(
            status.text,
            format!("dotfiles {} <span foreground=\"#a6e3a1\">\u{f120} Bash</span>", SEP_DARK)
        );
    }
}
