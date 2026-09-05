//! The account-limits section above the agent list.
//!
//! Presentation only: the figures, where they come from and how they are kept
//! fresh are [`crate::limits`]'s business. This decides the reading order, the
//! marks, the words and the colours — and re-exports what the window needs, so
//! the panel has one door to limits rather than two.
//!
//! Folded, the section is one row: the disclosure, then a chip per account
//! carrying its tightest window's figure, with the rest on hover. Unfolded, it
//! is T3 Code's usage hover card: one block per account, one summary line and
//! one meter per rolling window, with the same labels, colours and relative
//! reset text.

use std::cell::RefCell;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use gtk4 as gtk;
use gtk4::prelude::*;

use crate::limits::Window;
use crate::session::AgentKind;

pub use crate::limits::{read, Snapshot};

/// Reading order. A provider the cache names but this does not is appended
/// after rather than dropped.
const PROVIDER_ORDER: [&str; 2] = ["codex", "claude"];

/// Where a figure stops being a number and starts being a warning: peach from
/// the first, red from the second. Below that the provider's own colour, as in
/// T3's hover.
const PRESSURE_WARN: f64 = 60.0;
const PRESSURE_HOT: f64 = 85.0;

/// Hash of everything the section paints, so the poll loop can skip a rebuild.
pub fn fingerprint(snapshots: &[Snapshot], now: i64) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for snapshot in snapshots {
        snapshot.provider.hash(&mut h);
        snapshot.source.hash(&mut h);
        snapshot.account.hash(&mut h);
        snapshot.accent.hash(&mut h);
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

/// The CSS class a figure earns for how close it is to the ceiling.
fn pressure_class(used_percent: f64) -> Option<&'static str> {
    if used_percent >= PRESSURE_HOT {
        Some("pressure-hot")
    } else if used_percent >= PRESSURE_WARN {
        Some("pressure-warn")
    } else {
        None
    }
}

/// The window nearest to running out — the one figure the folded row shows.
fn tightest(snapshot: &Snapshot) -> Option<&Window> {
    snapshot
        .windows
        .iter()
        .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
}

fn provider_name(provider: &str) -> String {
    AgentKind::from_slug(provider)
        .as_ref()
        .map(AgentKind::display_name)
        .unwrap_or(provider)
        .to_string()
}

fn window_label(window: &Window, provider: &str) -> String {
    match window.id.as_str() {
        "five_hour" => "Session".to_string(),
        "seven_day" => "Weekly".to_string(),
        _ if provider == "claude" && !window.label.starts_with("Weekly · ") => {
            format!("Weekly · {}", window.label)
        }
        _ => window.label.clone(),
    }
}

/// The chip's hover: the block it stands for, as text.
fn tooltip_markup(snapshot: &Snapshot, now: i64) -> String {
    let esc = gtk::glib::markup_escape_text;
    let mut heading = provider_name(&snapshot.provider);
    if let Some(account) = &snapshot.account {
        heading.push_str(" · ");
        heading.push_str(account);
    }
    let mut text = format!("<b>{}</b>", esc(&heading));
    for window in &snapshot.windows {
        text.push('\n');
        text.push_str(&esc(&window_label(window, &snapshot.provider)));
        text.push_str(&format!(" · {}%", window.used_percent.round()));
        if let Some(reset) = format_resets_in(window.resets_at, now) {
            text.push_str(" · ");
            text.push_str(&reset);
        }
    }
    text
}

/// Providers in reading order, accounts in cache order, empties left out — the
/// same list for the chips and the blocks, so they can never disagree.
fn ordered(snapshots: &[Snapshot]) -> Vec<&Snapshot> {
    let reported = |s: &&Snapshot| !s.windows.is_empty();
    let mut out: Vec<&Snapshot> = Vec::new();
    for provider in PROVIDER_ORDER {
        out.extend(
            snapshots
                .iter()
                .filter(reported)
                .filter(|s| s.provider == provider),
        );
    }
    out.extend(
        snapshots
            .iter()
            .filter(reported)
            .filter(|s| !PROVIDER_ORDER.contains(&s.provider.as_str())),
    );
    out
}

// ---------------------------------------------------------------------------
// Widgets
// ---------------------------------------------------------------------------

/// The limits section: a disclosure row that remembers its state and carries
/// the per-account chips, over the per-account blocks it reveals.
pub struct Section {
    /// The whole thing, for the panel to append above the agent list.
    pub root: gtk::Box,
    /// The chips on the disclosure row, rebuilt with the body.
    summary: gtk::Box,
    /// The per-account blocks, thrown away and rebuilt on every data change.
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

        let summary = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        summary.add_css_class("limits-summary");
        summary.set_hexpand(true);
        summary.set_halign(gtk::Align::End);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
        body.add_css_class("limits-body");
        body.set_visible(crate::flags::LIMITS_EXPANDED.is_on());

        root.append(&disclosure(&root, &body, &summary, on_toggled));
        root.append(&body);
        // Nothing has been read yet, and an empty section must not flash on
        // the first open.
        root.set_visible(false);
        Self {
            root,
            summary,
            body,
        }
    }

    /// Repaint from a fresh read. Cheap enough to call on any change: the
    /// section is a handful of rows, and the poll loop gates it on a
    /// fingerprint anyway.
    pub fn rebuild(&self, snapshots: &[Snapshot], now: i64) {
        for container in [&self.summary, &self.body] {
            while let Some(child) = container.first_child() {
                container.remove(&child);
            }
        }
        let shown = ordered(snapshots);
        for snapshot in &shown {
            self.summary.append(&chip(snapshot, now));
            self.body.append(&provider_block(snapshot, now));
        }
        // A disclosure row over nothing is worse than silence.
        self.root.set_visible(!shown.is_empty());
    }
}

/// The clickable "Limits" row: a chevron that points at what the click will do
/// next, the chips, and a flag so the choice outlives the process.
fn disclosure(
    root: &gtk::Box,
    body: &gtk::Box,
    summary: &gtk::Box,
    on_toggled: impl Fn(&gtk::Box) + 'static,
) -> gtk::Button {
    let chevron = gtk::Image::new();
    let label = gtk::Label::new(Some("Limits"));
    label.add_css_class("limits-title");
    label.set_halign(gtk::Align::Start);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.append(&chevron);
    row.append(&label);
    row.append(summary);

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

/// One account on the folded row: its mark and its tightest figure, the rest
/// on hover. The pill-badge shape, so it reads as a badge rather than as a
/// second row of meters.
fn chip(snapshot: &Snapshot, now: i64) -> gtk::Box {
    let chip = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    chip.add_css_class("limit-chip");
    if let Some(mark) = account_mark(snapshot, 12) {
        chip.append(&mark);
    }
    if let Some(window) = tightest(snapshot) {
        chip.append(&percent_label(window, &snapshot.provider));
    }
    chip.set_tooltip_markup(Some(&tooltip_markup(snapshot, now)));
    chip
}

/// The provider's mark, wearing the account's accent as a badge when it has
/// one — T3's instance icon, so two Claude accounts read the same here as
/// there. `None` without an SVG loader, as for every mark.
fn account_mark(snapshot: &Snapshot, px: i32) -> Option<gtk::Widget> {
    let agent = AgentKind::from_slug(&snapshot.provider)?;
    let mark = super::svg_mark(agent.logo_svg(), px)?;
    let Some(accent) = snapshot.accent.as_deref() else {
        return Some(mark.upcast());
    };
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&mark));
    overlay.add_overlay(&accent_dot(accent));
    Some(overlay.upcast())
}

thread_local! {
    /// The accents already given a stylesheet, so each is installed once.
    static ACCENTS_STYLED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// A dot in the account's colour, for the corner of its mark.
///
/// GTK CSS has no per-widget inline colour, so each accent gets one class and
/// one provider on first sight. `accent` is `#rrggbb` by the time it reaches
/// the cache, which is what keeps it safe to drop into a stylesheet.
fn accent_dot(accent: &str) -> gtk::Box {
    let class = format!("accent-{}", accent.trim_start_matches('#'));
    ACCENTS_STYLED.with_borrow_mut(|styled| {
        if styled.insert(accent.to_string()) {
            let provider = gtk::CssProvider::new();
            provider.load_from_string(&format!(
                ".account-dot.{class} {{ background-color: {accent}; }}"
            ));
            if let Some(display) = gtk::gdk::Display::default() {
                gtk::style_context_add_provider_for_display(
                    &display,
                    &provider,
                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                );
            }
        }
    });
    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("account-dot");
    dot.add_css_class(&class);
    dot.set_halign(gtk::Align::End);
    dot.set_valign(gtk::Align::End);
    dot.set_can_target(false);
    dot
}

/// `41%`, in the provider's colour until pressure takes over.
fn percent_label(window: &Window, provider: &str) -> gtk::Label {
    let percent = gtk::Label::new(Some(&format!("{}%", window.used_percent.round())));
    percent.add_css_class("limit-percent");
    percent.add_css_class(provider);
    if let Some(class) = pressure_class(window.used_percent) {
        percent.add_css_class(class);
    }
    percent
}

/// One account's header and windows — the block the T3 hover card draws, at
/// the panel's width.
fn provider_block(snapshot: &Snapshot, now: i64) -> gtk::Box {
    let block = gtk::Box::new(gtk::Orientation::Vertical, 6);
    block.add_css_class("provider-block");

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    if let Some(mark) = account_mark(snapshot, 16) {
        header.append(&mark);
    }
    let name = gtk::Label::new(Some(&provider_name(&snapshot.provider)));
    name.add_css_class("provider-name");
    name.set_halign(gtk::Align::Start);
    header.append(&name);
    if let Some(account) = &snapshot.account {
        let instance = gtk::Label::new(Some(&format!("· {account}")));
        instance.add_css_class("provider-instance");
        instance.set_halign(gtk::Align::Start);
        instance.set_ellipsize(gtk::pango::EllipsizeMode::End);
        header.append(&instance);
    }
    block.append(&header);

    for window in &snapshot.windows {
        block.append(&window_row(window, &snapshot.provider, now));
    }
    block
}

/// One window: summary line first, then the full-width usage meter.
fn window_row(window: &Window, provider: &str, now: i64) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    row.add_css_class("limit-row");

    let summary = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    let label = gtk::Label::new(Some(&window_label(window, provider)));
    label.add_css_class("limit-window-label");
    label.set_hexpand(true);
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    summary.append(&label);
    summary.append(&percent_label(window, provider));

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
    if let Some(class) = pressure_class(window.used_percent) {
        meter.add_css_class(class);
    }
    meter.set_fraction((window.used_percent / 100.0).clamp(0.0, 1.0));
    meter.set_hexpand(true);
    row.append(&meter);
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(id: &str, used_percent: f64) -> Window {
        Window {
            id: id.to_string(),
            label: id.to_string(),
            used_percent,
            resets_at: Some(1_787_419_799),
        }
    }

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
            ..Default::default()
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
    fn the_chip_shows_the_window_nearest_to_running_out() {
        let snapshot = Snapshot {
            provider: "claude".to_string(),
            windows: vec![
                window("five_hour", 41.0),
                window("seven_day", 62.0),
                window("fable", 71.0),
            ],
            ..Default::default()
        };
        assert_eq!(tightest(&snapshot).map(|w| w.id.as_str()), Some("fable"));
        assert_eq!(tightest(&Snapshot::default()), None);
    }

    #[test]
    fn pressure_starts_at_peach_and_ends_in_red() {
        assert_eq!(pressure_class(59.9), None);
        assert_eq!(pressure_class(60.0), Some("pressure-warn"));
        assert_eq!(pressure_class(85.0), Some("pressure-hot"));
    }

    #[test]
    fn accounts_keep_the_provider_order_and_empties_stay_out() {
        let claude = |account: &str, windows: Vec<Window>| Snapshot {
            provider: "claude".to_string(),
            account: Some(account.to_string()),
            windows,
            ..Default::default()
        };
        let snapshots = vec![
            claude("Moinax", vec![window("seven_day", 8.0)]),
            claude("Mbrella", vec![window("seven_day", 62.0)]),
            claude("Quiet", Vec::new()),
            Snapshot {
                provider: "codex".to_string(),
                windows: vec![window("seven_day", 35.0)],
                ..Default::default()
            },
        ];
        let accounts: Vec<&str> = ordered(&snapshots)
            .iter()
            .map(|s| s.account.as_deref().unwrap_or(&s.provider))
            .collect();
        assert_eq!(accounts, ["codex", "Moinax", "Mbrella"]);
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

        // So does the account being renamed or recoloured in T3.
        let mut renamed = snapshots.clone();
        renamed[0].account = Some("Moinax".to_string());
        assert_ne!(base, fingerprint(&renamed, now));
    }
}
