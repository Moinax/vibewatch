use std::sync::OnceLock;

use crate::ipc::StatusResponse;
use crate::session::{Session, SessionStatus};

/// Per-status colors used by the panel (assets/palette-*.css).
struct Palette {
    green: &'static str,
    sapphire: &'static str,
    peach: &'static str,
    dim: &'static str,
}

const MOCHA: Palette = Palette {
    green: "#a6e3a1",
    sapphire: "#74c7ec",
    peach: "#fab387",
    dim: "#6c7086",
};

const LATTE: Palette = Palette {
    green: "#40a02b",
    sapphire: "#209fb5",
    peach: "#fe640b",
    dim: "#8c8fa1",
};

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
        SessionStatus::WaitingApproval => palette.peach,
        SessionStatus::Idle | SessionStatus::Stopped => palette.dim,
    }
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

    // Attention state skips the Pango color — its peach background (set in
    // the user's CSS) already carries the semantic.
    let decorate = |status: SessionStatus, raw: &str| -> String {
        let escaped = pango_escape(raw);
        if status == SessionStatus::WaitingApproval {
            escaped
        } else {
            format!(
                "<span foreground=\"{}\">{}</span>",
                color_for_status(status, palette),
                escaped
            )
        }
    };

    let text = if count == 0 {
        "\u{f544}".to_string()
    } else {
        let s = if count == 1 {
            active[0]
        } else {
            active.iter().max_by_key(|s| s.interest_priority()).unwrap()
        };
        let status_span = decorate(s.status, &s.inline_status());
        // All-idle: the specific session's display name carries no signal
        // (none of them are doing anything). Swap it for the app brand so
        // the widget identifies itself at rest.
        let name = if class == "idle" {
            "VibeWatch".to_string()
        } else {
            pango_escape(&s.display_name())
        };
        if count == 1 {
            format!("\u{f544} {} {}", name, status_span)
        } else {
            // Same colored ● used as the panel row indicator — matches
            // `.indicator.<status>` colors from palette-*.css.
            let dot = decorate(s.status, "\u{25cf}");
            format!("{} \u{f544} {} {} {}", count, dot, name, status_span)
        }
    };

    StatusResponse { text, class }
}

/// Print Waybar JSON to stdout. `class` is emitted as a single-element array
/// so waybar replaces the widget's class list each poll instead of
/// accumulating stale classes.
pub fn print_waybar_status(sessions: &[Session]) {
    let status = build_status(sessions);
    let waybar_json = serde_json::json!({
        "text": status.text,
        "class": [status.class],
    });
    println!("{}", waybar_json);
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

    #[test]
    fn test_empty_status() {
        let status = dark(&[]);
        assert_eq!(status.text, "\u{f544}");
        assert_eq!(status.class, "idle");
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
            "\u{f544} dotfiles <span foreground=\"#74c7ec\">thinking</span>"
        );
        assert_eq!(status.class, "active");
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
            "\u{f544} dotfiles <span foreground=\"#209fb5\">thinking</span>"
        );
    }

    #[test]
    fn test_executing_wins_over_thinking_in_multi() {
        // Executing beats Thinking via interest_priority, so the executing
        // session's name is the one shown (and the dot is green).
        let sessions = vec![
            make_named("dotfiles", AgentKind::ClaudeCode, SessionStatus::Thinking),
            make_named("vibewatch", AgentKind::Codex, SessionStatus::Executing),
        ];
        let status = dark(&sessions);
        assert_eq!(
            status.text,
            "2 \u{f544} <span foreground=\"#a6e3a1\">\u{25cf}</span> vibewatch <span foreground=\"#a6e3a1\">exec</span>"
        );
        assert_eq!(status.class, "active");
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
        assert_eq!(status.text, "\u{f544} dotfiles approval");
    }

    #[test]
    fn test_stopped_sessions_excluded_from_count() {
        let sessions = vec![
            make_named("dotfiles", AgentKind::ClaudeCode, SessionStatus::Thinking),
            make_named("vibewatch", AgentKind::Codex, SessionStatus::Stopped),
        ];
        let status = dark(&sessions);
        assert_eq!(
            status.text,
            "\u{f544} dotfiles <span foreground=\"#74c7ec\">thinking</span>"
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
            "\u{f544} VibeWatch <span foreground=\"#6c7086\">idle</span>"
        );
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
            "2 \u{f544} <span foreground=\"#6c7086\">\u{25cf}</span> VibeWatch <span foreground=\"#6c7086\">idle</span>"
        );
    }

    #[test]
    fn test_pango_escape_in_tool_name() {
        let mut session = make_named("dotfiles", AgentKind::ClaudeCode, SessionStatus::Executing);
        session.current_tool = Some("A&B<x>".to_string());
        let status = dark(&[session]);
        assert_eq!(
            status.text,
            "\u{f544} dotfiles <span foreground=\"#a6e3a1\">A&amp;B&lt;x&gt;</span>"
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
            "\u{f544} dotfiles <span foreground=\"#a6e3a1\">Bash</span>"
        );
    }
}
