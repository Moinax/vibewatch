//! The account-limits section above the agent list.
//!
//! Presentation only: the figures, where they come from and how they are kept
//! fresh are [`crate::limits`]'s business. This decides the reading order, the
//! marks, the words and the colours — and re-exports what the window needs, so
//! the panel has one door to limits rather than two.
//!
//! Modelled on T3 Code's usage hover card: one block per provider, one summary
//! line and one meter per rolling window, with the same labels, colours and
//! relative reset text.

use std::hash::{Hash, Hasher};

use gtk4 as gtk;
use gtk4::prelude::*;

use crate::session::AgentKind;

pub use crate::limits::{read, Snapshot};

/// Reading order. A provider the cache names but this does not is appended
/// after rather than dropped.
const PROVIDER_ORDER: [&str; 2] = ["codex", "claude"];

/// Hash of everything the section paints, so the poll loop can skip a rebuild.
pub fn fingerprint(snapshots: &[Snapshot], now: i64) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for snapshot in snapshots {
        snapshot.provider.hash(&mut h);
        for window in &snapshot.windows {
            window.id.hash(&mut h);
            window.label.hash(&mut h);
            window.used_percent.to_bits().hash(&mut h);
            // Hash the rendered countdown so it updates at its own minute/hour
            // precision rather than on every 100 ms panel poll.
            format_resets_in(window.resets_at, now).hash(&mut h);
        }
    }
    h.finish()
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// The compact duration used by T3 Code's limits hover.
pub fn format_duration(seconds: i64) -> String {
    let remaining = seconds.max(0);
    let days = remaining / 86_400;
    let hours = (remaining % 86_400) / 3_600;
    let minutes = (remaining % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

/// `resets in 2h 13m`, `resets now`, or nothing without a reset clock.
pub fn format_resets_in(resets_at: Option<i64>, now: i64) -> Option<String> {
    let at = resets_at?;
    if at <= now {
        return Some("resets now".to_string());
    }
    Some(format!("resets in {}", format_duration(at - now)))
}

// ---------------------------------------------------------------------------
// Widgets
// ---------------------------------------------------------------------------

/// The limits section: a disclosure row that remembers its state, over the
/// per-provider blocks it reveals.
pub struct Section {
    /// The whole thing, for the panel to append above the agent list.
    pub root: gtk::Box,
    /// The per-provider blocks, thrown away and rebuilt on every data change.
    body: gtk::Box,
}

impl Section {
    /// `on_toggled` runs after the body's visibility flips, with the section's
    /// root, and must re-cap and resize around its new height. Nothing else
    /// will: the poll loop only relayouts when data changes, and a click is not
    /// data.
    pub fn new(on_toggled: impl Fn(&gtk::Box) + 'static) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("limits-section");

        let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
        body.add_css_class("limits-body");
        body.set_visible(crate::flags::LIMITS_EXPANDED.is_on());

        root.append(&disclosure(&root, &body, on_toggled));
        root.append(&body);
        // Nothing has been read yet, and an empty section must not flash on
        // the first open.
        root.set_visible(false);
        Self { root, body }
    }

    /// Repaint from a fresh read. Cheap enough to call on any change: the
    /// section is a handful of rows, and the poll loop gates it on a
    /// fingerprint anyway.
    pub fn rebuild(&self, snapshots: &[Snapshot], now: i64) {
        while let Some(child) = self.body.first_child() {
            self.body.remove(&child);
        }
        // Match T3's hover: only providers with reported limits get a block.
        for provider in PROVIDER_ORDER {
            if let Some(snapshot) = snapshots
                .iter()
                .find(|s| s.provider == provider && !s.windows.is_empty())
            {
                self.body.append(&provider_block(provider, snapshot, now));
            }
        }
        for snapshot in snapshots {
            if !snapshot.windows.is_empty() && !PROVIDER_ORDER.contains(&snapshot.provider.as_str())
            {
                self.body
                    .append(&provider_block(&snapshot.provider, snapshot, now));
            }
        }
        // A disclosure row over nothing is worse than silence.
        self.root.set_visible(
            snapshots
                .iter()
                .any(|snapshot| !snapshot.windows.is_empty()),
        );
    }
}

/// The clickable "Limits" row: a chevron that points at what the click will do
/// next, and a flag so the choice outlives the process.
fn disclosure(
    root: &gtk::Box,
    body: &gtk::Box,
    on_toggled: impl Fn(&gtk::Box) + 'static,
) -> gtk::Button {
    let chevron = gtk::Image::new();
    let label = gtk::Label::new(Some("Limits"));
    label.add_css_class("limits-title");
    label.set_halign(gtk::Align::Start);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.append(&chevron);
    row.append(&label);

    let button = gtk::Button::new();
    button.add_css_class("limits-toggle");
    button.add_css_class("flat");
    button.set_child(Some(&row));

    // Paints from what the flag reads back rather than from what was asked for,
    // so a write that never landed cannot leave the chevron lying — same
    // contract as the header toggles.
    let paint = {
        let chevron = chevron.clone();
        let body = body.clone();
        move |expanded: bool| {
            chevron.set_icon_name(Some(if expanded {
                "pan-down-symbolic"
            } else {
                "pan-end-symbolic"
            }));
            body.set_visible(expanded);
        }
    };
    paint(crate::flags::LIMITS_EXPANDED.is_on());
    let root = root.clone();
    button.connect_clicked(move |_| {
        paint(crate::flags::LIMITS_EXPANDED.toggle());
        // The section has already changed size; the list's ceiling and the
        // window have not.
        on_toggled(&root);
    });
    button
}

/// One provider's header and windows — the block the T3 hover card draws, at
/// the panel's width.
fn provider_block(provider: &str, snapshot: &Snapshot, now: i64) -> gtk::Box {
    let block = gtk::Box::new(gtk::Orientation::Vertical, 6);
    block.add_css_class("provider-block");

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let agent = AgentKind::from_slug(provider);
    if let Some(mark) = agent.and_then(|a| super::svg_mark(a.logo_svg(), 16)) {
        header.append(&mark);
    }
    let name = gtk::Label::new(Some(
        agent
            .as_ref()
            .map(AgentKind::display_name)
            .unwrap_or(provider),
    ));
    name.add_css_class("provider-name");
    name.set_halign(gtk::Align::Start);
    header.append(&name);
    block.append(&header);

    for window in &snapshot.windows {
        block.append(&window_row(window, provider, now));
    }
    block
}

fn window_label(window: &crate::limits::Window, provider: &str) -> String {
    match window.id.as_str() {
        "five_hour" => "Session".to_string(),
        "seven_day" => "Weekly".to_string(),
        _ if provider == "claude" && !window.label.starts_with("Weekly · ") => {
            format!("Weekly · {}", window.label)
        }
        _ => window.label.clone(),
    }
}

/// One window: summary line first, then the full-width usage meter.
fn window_row(window: &crate::limits::Window, provider: &str, now: i64) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    row.add_css_class("limit-row");

    let summary = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    let label = gtk::Label::new(Some(&window_label(window, provider)));
    label.add_css_class("limit-window-label");
    label.set_hexpand(true);
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    summary.append(&label);

    let percent = gtk::Label::new(Some(&format!("{}%", window.used_percent.round())));
    percent.add_css_class("limit-percent");
    percent.add_css_class(provider);
    summary.append(&percent);

    if let Some(reset) = format_resets_in(window.resets_at, now) {
        let countdown = gtk::Label::new(Some(&reset));
        countdown.add_css_class("limit-reset");
        countdown.set_width_chars(17);
        countdown.set_xalign(1.0);
        summary.append(&countdown);
    }
    row.append(&summary);

    let meter = gtk::ProgressBar::new();
    meter.add_css_class("limit-meter");
    meter.add_css_class(provider);
    meter.set_fraction((window.used_percent / 100.0).clamp(0.0, 1.0));
    meter.set_hexpand(true);
    row.append(&meter);
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::Window;

    fn snapshots() -> Vec<Snapshot> {
        vec![Snapshot {
            provider: "claude".to_string(),
            windows: vec![Window {
                id: "five_hour".to_string(),
                label: "5h".to_string(),
                used_percent: 9.0,
                resets_at: Some(1_787_419_799),
            }],
            as_of: 1_787_406_101,
        }]
    }

    #[test]
    fn reset_countdowns_match_the_t3_hover_format() {
        let now = 1_787_406_101;
        assert_eq!(
            format_resets_in(Some(now + 2 * 3600 + 15 * 60), now).as_deref(),
            Some("resets in 2h 15m")
        );
        assert_eq!(
            format_resets_in(Some(now + 3 * 86_400 + 4 * 3600), now).as_deref(),
            Some("resets in 3d 4h")
        );
        assert_eq!(
            format_resets_in(Some(now - 60), now).as_deref(),
            Some("resets now")
        );
        assert_eq!(format_resets_in(None, now), None);
    }

    #[test]
    fn cached_legacy_labels_are_normalized_for_the_t3_layout() {
        let session = Window {
            id: "five_hour".to_string(),
            label: "5h".to_string(),
            used_percent: 9.0,
            resets_at: None,
        };
        let scoped = Window {
            id: "fable".to_string(),
            label: "Fable".to_string(),
            used_percent: 1.0,
            resets_at: None,
        };

        assert_eq!(window_label(&session, "claude"), "Session");
        assert_eq!(window_label(&scoped, "claude"), "Weekly · Fable");
    }

    #[test]
    fn the_fingerprint_tracks_the_paint_and_not_the_clock() {
        let snapshots = snapshots();
        let now = snapshots[0].as_of;
        let base = fingerprint(&snapshots, now);

        assert_eq!(base, fingerprint(&snapshots, now + 1));
        assert_ne!(base, fingerprint(&snapshots, now + 60));

        // A reset clock running out also changes the rendered caption.
        let after_reset = snapshots[0].windows[0].resets_at.expect("a clock") + 1;
        assert_ne!(
            fingerprint(&snapshots, after_reset - 120),
            fingerprint(&snapshots, after_reset)
        );

        let mut moved = snapshots.clone();
        moved[0].windows[0].used_percent = 10.0;
        assert_ne!(base, fingerprint(&moved, now));
    }
}
