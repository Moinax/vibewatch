//! The account-limits section above the agent list.
//!
//! Presentation only: the figures, where they come from and how they are kept
//! fresh are [`crate::limits`]'s business. This decides the reading order, the
//! marks, the words and the colours — and re-exports what the window needs, so
//! the panel has one door to limits rather than two.
//!
//! Modelled on T3 Code's usage hover card, which is where the layout was
//! settled: one block per provider, one row per rolling window, and the used
//! share spelled out because a bare percentage cannot say whether it is the
//! part spent or the part left.

use std::hash::{Hash, Hasher};

use gtk4 as gtk;
use gtk4::prelude::*;

use crate::session::AgentKind;

pub use crate::limits::{read, Snapshot};

/// Reading order. A provider the cache names but this does not is appended
/// after rather than dropped.
const PROVIDER_ORDER: [&str; 2] = ["codex", "claude"];

/// Age past which a snapshot stops being "current" and earns a caption.
///
/// Claude's figures come off a request that only fires on a finished turn or a
/// panel open, and Codex's off whenever it last ran, so both can be hours old
/// with nothing on screen saying so — which is the one lie worth spending a
/// line of the card on.
const STALE_AFTER_SECS: i64 = 15 * 60;

/// Used-share past which the figure is worth colouring.
const WARN_PERCENT: f64 = 80.0;
const CRITICAL_PERCENT: f64 = 95.0;

/// The loudest tone any window is wearing, or none while they are all calm.
///
/// Folded through [`usage_tone`] rather than comparing tones, so the section's
/// mark and the row that earned it can never disagree about where the
/// thresholds are.
fn worst_tone(snapshots: &[Snapshot]) -> Option<&'static str> {
    let worst = snapshots
        .iter()
        .flat_map(|snapshot| &snapshot.windows)
        .map(|window| window.used_percent)
        .fold(f64::NAN, f64::max);
    usage_tone(worst)
}

/// Hash of everything the section paints, so the poll loop can skip a rebuild.
///
/// The captions are hashed as rendered, and not the clock they are derived
/// from. Hashing the minute instead made this differ every 60 seconds whatever
/// the data said, so a panel left open tore the whole section down and resized
/// its window once a minute to paint the identical pixels — and did it while
/// the section was folded shut and none of it was on screen.
pub fn fingerprint(snapshots: &[Snapshot], now: i64) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for snapshot in snapshots {
        snapshot.provider.hash(&mut h);
        format_age(snapshot.as_of, now).hash(&mut h);
        for window in &snapshot.windows {
            window.id.hash(&mut h);
            window.label.hash(&mut h);
            window.used_percent.to_bits().hash(&mut h);
            // As rendered, for the same reason: `now` crossing a reset turns a
            // clock time into "now", and nothing else about the window moves.
            format_reset_at(window.resets_at, now).hash(&mut h);
        }
    }
    h.finish()
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// When a window resets, on the reader's own clock: a bare time inside 24
/// hours, date and time beyond.
///
/// `None` for a window with no clock yet. `"now"` once the moment has passed,
/// which does happen — providers refresh the figure lazily, so a spent
/// timestamp lingers for a while after the window has actually rolled over.
///
/// Through glib, which is already linked here and is the piece that knows the
/// local zone. The crate's own clock is Unix seconds, so nothing is parsed.
pub fn format_reset_at(resets_at: Option<i64>, now: i64) -> Option<String> {
    let at = resets_at?;
    if at <= now {
        return Some("now".to_string());
    }
    let local = gtk::glib::DateTime::from_unix_local(at).ok()?;
    let pattern = if at - now < 24 * 3600 {
        "%H:%M"
    } else {
        "%-m/%-d %H:%M"
    };
    local.format(pattern).ok().map(|s| s.to_string())
}

/// How long ago the provider reported, and only once that is worth saying —
/// fresh data needs no caption.
pub fn format_age(as_of: i64, now: i64) -> Option<String> {
    let secs = now - as_of;
    if secs < STALE_AFTER_SECS {
        return None;
    }
    Some(if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    })
}

/// The CSS class a used-share earns, or none while it is unremarkable.
pub fn usage_tone(used_percent: f64) -> Option<&'static str> {
    if used_percent >= CRITICAL_PERCENT {
        Some("limit-critical")
    } else if used_percent >= WARN_PERCENT {
        Some("limit-warn")
    } else {
        None
    }
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
    /// Warns from the disclosure row that some window has gone hot.
    ///
    /// It lives up there, and not among the meters, because folded is exactly
    /// when it earns its place: the row is all that is left on screen, so it is
    /// the only thing that can say there is something worth opening for.
    warning: gtk::Image,
}

impl Section {
    /// `on_toggled` runs after the body's visibility flips, with the section's
    /// root, and must re-cap and resize around its new height. Nothing else
    /// will: the poll loop only relayouts when data changes, and a click is not
    /// data.
    pub fn new(on_toggled: impl Fn(&gtk::Box) + 'static) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("limits-section");

        let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
        body.add_css_class("limits-body");
        body.set_visible(crate::flags::LIMITS_EXPANDED.is_on());

        let warning = gtk::Image::from_icon_name("dialog-warning-symbolic");
        warning.add_css_class("limit-warning");
        warning.set_visible(false);
        // Takes the row's slack while hugging the title, so the mark reads as
        // part of "Limits" rather than as a second thing at the far end.
        warning.set_hexpand(true);
        warning.set_halign(gtk::Align::Start);

        root.append(&disclosure(&root, &body, &warning, on_toggled));
        root.append(&body);
        // Nothing has been read yet, and an empty section must not flash on
        // the first open.
        root.set_visible(false);
        Self {
            root,
            body,
            warning,
        }
    }

    /// Repaint from a fresh read. Cheap enough to call on any change: the
    /// section is a handful of rows, and the poll loop gates it on a
    /// fingerprint anyway.
    pub fn rebuild(&self, snapshots: &[Snapshot], now: i64) {
        while let Some(child) = self.body.first_child() {
            self.body.remove(&child);
        }
        // Every known provider gets a block whether or not it reported, so a
        // provider that has simply never run says so rather than vanishing.
        // One the cache names and this does not still gets its own.
        for provider in PROVIDER_ORDER {
            let snapshot = snapshots.iter().find(|s| s.provider == provider);
            self.body.append(&provider_block(provider, snapshot, now));
        }
        for snapshot in snapshots {
            if !PROVIDER_ORDER.contains(&snapshot.provider.as_str()) {
                self.body
                    .append(&provider_block(&snapshot.provider, Some(snapshot), now));
            }
        }
        // Cleared before it is set: the classes accumulate across rebuilds
        // otherwise, and a window cooling off would leave the mark red.
        let tone = worst_tone(snapshots);
        self.warning.remove_css_class("limit-warn");
        self.warning.remove_css_class("limit-critical");
        if let Some(tone) = tone {
            self.warning.add_css_class(tone);
        }
        self.warning.set_visible(tone.is_some());

        // A disclosure row over nothing is worse than silence: with no provider
        // reporting — nothing has run yet, or the very first fetch has not
        // landed — the section is not there at all.
        self.root.set_visible(!snapshots.is_empty());
    }
}

/// The clickable "Limits" row: a chevron that points at what the click will do
/// next, and a flag so the choice outlives the process.
fn disclosure(
    root: &gtk::Box,
    body: &gtk::Box,
    warning: &gtk::Image,
    on_toggled: impl Fn(&gtk::Box) + 'static,
) -> gtk::Button {
    let chevron = gtk::Image::new();
    let label = gtk::Label::new(Some("Limits"));
    label.add_css_class("limits-title");
    label.set_halign(gtk::Align::Start);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.append(&chevron);
    row.append(&label);
    row.append(warning);

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
fn provider_block(provider: &str, snapshot: Option<&Snapshot>, now: i64) -> gtk::Box {
    let block = gtk::Box::new(gtk::Orientation::Vertical, 3);
    block.add_css_class("provider-block");

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let agent = AgentKind::from_slug(provider);
    if let Some(mark) = agent.and_then(|a| super::svg_mark(a.logo_svg(), 12)) {
        header.append(&mark);
    }
    let name = gtk::Label::new(Some(
        agent
            .as_ref()
            .map(AgentKind::display_name)
            .unwrap_or(provider),
    ));
    name.add_css_class("provider-name");
    name.set_hexpand(true);
    name.set_halign(gtk::Align::Start);
    header.append(&name);

    if let Some(age) = snapshot.and_then(|s| format_age(s.as_of, now)) {
        let caption = gtk::Label::new(Some(&age));
        caption.add_css_class("limit-age");
        header.append(&caption);
    }
    block.append(&header);

    match snapshot {
        Some(snapshot) if !snapshot.windows.is_empty() => {
            for window in &snapshot.windows {
                block.append(&window_row(window, provider, now));
            }
        }
        _ => {
            let empty = gtk::Label::new(Some("No limit data yet"));
            empty.add_css_class("limit-empty");
            empty.set_halign(gtk::Align::Start);
            block.append(&empty);
        }
    }
    block
}

/// One window: its name, how full it is, and when it rolls over.
fn window_row(window: &crate::limits::Window, provider: &str, now: i64) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.add_css_class("limit-row");

    let label = gtk::Label::new(Some(&window.label));
    label.add_css_class("limit-window-label");
    label.set_width_chars(4);
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    row.append(&label);

    let meter = gtk::ProgressBar::new();
    meter.add_css_class("limit-meter");
    // Colours the fill per provider, the same distinction the T3 card makes.
    meter.add_css_class(provider);
    meter.set_fraction((window.used_percent / 100.0).clamp(0.0, 1.0));
    meter.set_hexpand(true);
    meter.set_valign(gtk::Align::Center);
    row.append(&meter);

    // Spelled out, because a bare percentage cannot say whether it is the share
    // spent or the share left.
    let used = gtk::Label::new(Some(&format!("{}% used", window.used_percent.round())));
    used.add_css_class("limit-used");
    if let Some(tone) = usage_tone(window.used_percent) {
        used.add_css_class(tone);
    }
    row.append(&used);

    if let Some(reset) = format_reset_at(window.resets_at, now) {
        let at = gtk::Label::new(Some(&reset));
        at.add_css_class("limit-reset");
        row.append(&at);
    }
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
    fn a_stale_snapshot_earns_a_caption_and_a_fresh_one_does_not() {
        let now = 1_787_406_101;
        assert_eq!(format_age(now, now), None);
        assert_eq!(format_age(now - 600, now), None);
        assert_eq!(format_age(now - 3600, now).as_deref(), Some("1h ago"));
        assert_eq!(format_age(now - 2 * 86_400, now).as_deref(), Some("2d ago"));
    }

    #[test]
    fn a_reset_clock_reads_as_a_time_today_and_a_date_beyond() {
        let now = 1_787_406_101;
        let soon = format_reset_at(Some(now + 3600), now);
        assert!(
            soon.as_deref()
                .is_some_and(|s| s.len() == 5 && s.contains(':')),
            "a window resetting today reads as a bare clock time, got {soon:?}"
        );
        let later = format_reset_at(Some(now + 3 * 86_400), now);
        assert!(
            later.as_deref().is_some_and(|s| s.contains('/')),
            "a window resetting past 24h carries its date, got {later:?}"
        );
        // A spent clock, which providers leave lying around after a rollover.
        assert_eq!(format_reset_at(Some(now - 60), now).as_deref(), Some("now"));
        assert_eq!(format_reset_at(None, now), None);
    }

    #[test]
    fn only_a_share_worth_worrying_about_is_coloured() {
        assert_eq!(usage_tone(0.85), None);
        assert_eq!(usage_tone(79.9), None);
        assert_eq!(usage_tone(80.0), Some("limit-warn"));
        assert_eq!(usage_tone(95.0), Some("limit-critical"));
    }

    #[test]
    fn the_mark_lights_on_the_hottest_window_anywhere() {
        let mut snapshots = snapshots();
        assert_eq!(worst_tone(&snapshots), None, "9% is nobody's problem");

        // A second provider's window counts too — the mark speaks for the
        // whole section, not for the first block in it.
        snapshots.push(Snapshot {
            provider: "codex".to_string(),
            windows: vec![Window {
                id: "seven_day".to_string(),
                label: "Week".to_string(),
                used_percent: 84.0,
                resets_at: None,
            }],
            as_of: 0,
        });
        assert_eq!(worst_tone(&snapshots), Some("limit-warn"));

        // The loudest wins, not the last read.
        snapshots[0].windows[0].used_percent = 97.0;
        assert_eq!(worst_tone(&snapshots), Some("limit-critical"));

        assert_eq!(worst_tone(&[]), None, "no data is not a warning");
    }

    #[test]
    fn the_fingerprint_tracks_the_paint_and_not_the_clock() {
        let snapshots = snapshots();
        let now = snapshots[0].as_of;
        let base = fingerprint(&snapshots, now);

        // A minute of wall clock on fresh data paints nothing new, and must
        // not cost a teardown.
        assert_eq!(base, fingerprint(&snapshots, now + 60));
        assert_eq!(base, fingerprint(&snapshots, now + STALE_AFTER_SECS - 1));

        // Crossing into staleness does put a caption on screen.
        assert_ne!(base, fingerprint(&snapshots, now + STALE_AFTER_SECS));

        // As does a reset clock running out.
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
