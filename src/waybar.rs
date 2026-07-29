use std::sync::OnceLock;

use crate::ipc::StatusResponse;
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
}

const MOCHA: Palette = Palette {
    green: "#a6e3a1",
    sapphire: "#74c7ec",
    dim: "#6c7086",
    attention_text: "#94e2d5",
    peach: "#fab387",
};

const LATTE: Palette = Palette {
    green: "#40a02b",
    sapphire: "#209fb5",
    dim: "#8c8fa1",
    attention_text: "#179299",
    peach: "#fe640b",
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
/// Marks the finished turn, alongside the peach (`\u{2714}`, heavy check).
const FINISH_MARK: &str = "\u{2714}";

/// Cached once per process — theme toggles require a daemon restart, which
/// is acceptable given this runs on a waybar-driven 2s poll cadence.
fn active_palette() -> &'static Palette {
    static DARK: OnceLock<bool> = OnceLock::new();
    if *DARK.get_or_init(detect_dark_mode) {
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

/// The state word and its color for the session in the lead. A just-finished
/// turn is `Idle` as far as `status` is concerned, so it has to be asked about
/// separately or the finish reads as "nothing happening".
fn headline_status(session: &Session, palette: &Palette) -> String {
    if session.just_finished() {
        return tint(palette.peach, &format!("{} done", FINISH_MARK));
    }
    tint(
        color_for_status(session.status, palette),
        &session.inline_status(),
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
        // One span, not three of the same colour: the whole phrase is dim.
        let text = if count == 0 {
            tint(palette.dim, BRAND_NAME)
        } else {
            tint(
                palette.dim,
                &format!("{} \u{00b7} {} idle", BRAND_NAME, count),
            )
        };
        return StatusResponse {
            text,
            class,
            logo: LOGO_BRAND.to_string(),
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

    let mut text = format!(
        "{} {} {}",
        pango_escape(&truncate_name(&lead.display_name())),
        tint(palette.dim, NAME_SEP),
        headline_status(lead, palette),
    );
    // The others, as a badge rather than a bare leading count: the old `4` sat
    // in front promising four agents while showing one, reading as part of the
    // name.
    if count > 1 {
        text.push_str("  ");
        text.push_str(&tint(palette.dim, &format!("+{}", count - 1)));
    }

    StatusResponse {
        text,
        class,
        logo: lead.agent.logo_class().to_string(),
    }
}

/// Shape a `StatusResponse` as the JSON payload waybar consumes. The classes go
/// out as a whole array so waybar replaces the widget's class list each update
/// instead of accumulating stale ones — which is also why the logo has to ride
/// along in the same array rather than being set once and left alone.
pub fn payload(status: &StatusResponse) -> String {
    serde_json::json!({
        "text": status.text,
        "class": [status.class.as_str(), status.logo.as_str()],
    })
    .to_string()
}

/// Print Waybar JSON to stdout.
pub fn print_waybar_status(sessions: &[Session]) {
    println!("{}", payload(&build_status(sessions)));
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
        assert_eq!(status.text, "<span foreground=\"#6c7086\">VibeWatch</span>");
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
                "dotfiles {} <span foreground=\"#74c7ec\">thinking</span>",
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
             <span foreground=\"#209fb5\">thinking</span>"
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
                "vibewatch {} <span foreground=\"#a6e3a1\">exec</span>  \
                 <span foreground=\"#6c7086\">+1</span>",
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
                 <span foreground=\"#6c7086\">+1</span>",
                SEP_DARK
            )
        );
        assert_eq!(status.logo, "logo-claude");
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
                "dotfiles {} <span foreground=\"#94e2d5\">awaiting approval</span>",
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
                "dotfiles {} <span foreground=\"#74c7ec\">thinking</span>",
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
            "<span foreground=\"#6c7086\">VibeWatch \u{00b7} 1 idle</span>"
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
            "<span foreground=\"#6c7086\">VibeWatch \u{00b7} 2 idle</span>"
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
            "<span foreground=\"#6c7086\">VibeWatch \u{00b7} 1 idle</span>"
        );
    }

    #[test]
    fn payload_carries_the_state_and_the_logo_in_one_class_list() {
        let sessions = vec![make_named(
            "dotfiles",
            AgentKind::ClaudeCode,
            SessionStatus::Executing,
        )];
        let json = payload(&dark(&sessions));
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
                "dotfiles {} <span foreground=\"#a6e3a1\">A&amp;B&lt;x&gt;</span>",
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
            format!("dotfiles {} <span foreground=\"#a6e3a1\">Bash</span>", SEP_DARK)
        );
    }
}
