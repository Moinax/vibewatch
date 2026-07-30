use std::cell::Cell;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4 as gtk;
use gtk4_layer_shell::LayerShell;
use libadwaita as adw;

use adw::prelude::*;

use crate::config::PanelConfig;
use crate::session::{Session, SessionRegistry, SessionStatus};

use super::session_row;

/// Hash the panel-visible fields of every session. The 10 Hz timer uses this
/// to skip rebuilds when nothing the panel renders has changed — far cheaper
/// than the previous full-JSON-serialize-and-compare.
fn sessions_fingerprint(sessions: &[Session]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    sessions.len().hash(&mut h);
    for s in sessions {
        s.id.hash(&mut h);
        s.status.hash(&mut h);
        s.current_tool.hash(&mut h);
        s.tool_detail.hash(&mut h);
        s.last_tool.hash(&mut h);
        s.last_tool_detail.hash(&mut h);
        s.last_tool_at.hash(&mut h);
        s.last_prompt.hash(&mut h);
        s.last_prompt_at.hash(&mut h);
        s.last_agent_text.hash(&mut h);
        s.last_agent_text_at.hash(&mut h);
        s.session_name.hash(&mut h);
        s.terminal.hash(&mut h);
        s.started_at_epoch.hash(&mut h);
        // Derived from status + `finished_at`, not stored, and not time-based:
        // it clears when the click acknowledges the finish, and when the agent
        // picks the work back up. Hashed so the card stops being lit on the
        // very next poll instead of staying lit until something else changes.
        s.just_finished().hash(&mut h);
        s.pending_approval
            .as_ref()
            .map(|p| (&p.request_id, p.choices.len()))
            .hash(&mut h);
    }
    h.finish()
}

thread_local! {
    /// When the panel last had a reason to be up. The auto-close countdown
    /// measures from here. It lives outside `build_window` so [`show`] can
    /// restamp it: an agent finishing a second before the close deadline pops
    /// the drawer open, and it must then get its full dwell time rather than
    /// snapping shut on the previous event's clock.
    ///
    /// Process-wide rather than window-owned, which is sound only because the
    /// daemon builds exactly one panel — the second `activate` is refused in
    /// `run_daemon_with_panel`. Everything here runs on the GTK main thread.
    static ALIVE_SINCE: Cell<Instant> = Cell::new(Instant::now());
}

/// Restart the auto-close countdown.
fn keep_alive() {
    ALIVE_SINCE.with(|c| c.set(Instant::now()));
}

/// How long the panel has been up with nothing asking it to stay.
fn alive_elapsed() -> Duration {
    ALIVE_SINCE.with(|c| c.get().elapsed())
}

/// vibewatch's own mark, beside the panel title — the same file the waybar pill
/// paints, embedded rather than read from `~/.config/vibewatch/logos/` so the
/// panel does not depend on `vibewatch install` having run.
///
/// `None` when the SVG cannot be decoded, which means no gdk-pixbuf SVG loader
/// (librsvg) on the system. The header then simply has no mark: a missing
/// decoration must never be the reason a panel fails to build.
fn brand_mark() -> Option<gtk::Image> {
    const MARK: &[u8] = include_bytes!("../../assets/logos/vibewatch.svg");
    let stream = gtk::gio::MemoryInputStream::from_bytes(&gtk::glib::Bytes::from_static(MARK));
    // Rasterised at 2x the display size so it stays clean on a HiDPI output.
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_stream_at_scale(
        &stream,
        32,
        32,
        true,
        gtk::gio::Cancellable::NONE,
    )
    .ok()?;
    let image = gtk::Image::from_paintable(Some(&gtk::gdk::Texture::for_pixbuf(&pixbuf)));
    image.set_pixel_size(16);
    Some(image)
}

/// Set the mute button's icon and tooltip to match the current state.
fn apply_mute_icon(btn: &gtk::Button, muted: bool) {
    btn.set_icon_name(if muted {
        "audio-volume-muted-symbolic"
    } else {
        "audio-volume-high-symbolic"
    });
    btn.set_tooltip_text(Some(if muted {
        "Sound muted — click to unmute"
    } else {
        "Sound on — click to mute"
    }));
}

pub fn build_window(
    app: &adw::Application,
    registry: SessionRegistry,
    panel_cfg: PanelConfig,
) -> adw::ApplicationWindow {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("vibewatch")
        .build();
    // Set only width, let height be driven by content
    window.set_size_request(360, 1);

    // Layer shell setup — anchor top only so the compositor centers us horizontally.
    window.init_layer_shell();
    window.set_layer(gtk4_layer_shell::Layer::Overlay);
    window.set_anchor(gtk4_layer_shell::Edge::Top, true);
    window.set_margin(gtk4_layer_shell::Edge::Top, 14);
    window.set_exclusive_zone(0);
    // `None` so the layer surface never steals keyboard focus from the
    // focused terminal. The panel is mouse-only (GestureClick on rows,
    // connect_clicked on buttons) — no widgets consume keyboard input.
    window.set_keyboard_mode(gtk4_layer_shell::KeyboardMode::None);
    window.set_namespace(Some("vibewatch"));

    // Load CSS — palette provider is swapped on OS dark/light theme change.
    let display = gtk::gdk::Display::default().unwrap();

    let palette_provider = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(
        &display,
        &palette_provider,
        gtk::STYLE_PROVIDER_PRIORITY_USER,
    );

    let style_provider = gtk::CssProvider::new();
    style_provider.load_from_string(include_str!("../../assets/style.css"));
    gtk::style_context_add_provider_for_display(
        &display,
        &style_provider,
        gtk::STYLE_PROVIDER_PRIORITY_USER,
    );

    const PALETTE_MOCHA: &str = include_str!("../../assets/palette-mocha.css");
    const PALETTE_LATTE: &str = include_str!("../../assets/palette-latte.css");
    /// Adopt a colour scheme across both surfaces vibewatch paints.
    ///
    /// The panel's half is CSS. The bar's half is not: those colours are inline
    /// Pango the daemon writes into its own payload, out of reach of any
    /// stylesheet, so the flavour has to be pushed into `waybar` by hand. GTK is
    /// the right place to push it from — it is the one component here that hears
    /// the portal's scheme changes live, and the daemon owns both surfaces.
    fn apply_scheme(provider: &gtk::CssProvider, dark: bool) {
        provider.load_from_string(if dark { PALETTE_MOCHA } else { PALETTE_LATTE });
        crate::waybar::set_dark_mode(dark);
    }

    let style_manager = adw::StyleManager::default();
    apply_scheme(&palette_provider, style_manager.is_dark());
    let palette_for_notify = palette_provider.clone();
    style_manager.connect_dark_notify(move |sm| {
        apply_scheme(&palette_for_notify, sm.is_dark());
    });

    // Main layout box
    let main_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    main_box.add_css_class("main-box");
    main_box.set_vexpand(false);
    main_box.set_size_request(360, -1);
    main_box.set_hexpand(false);
    main_box.set_halign(gtk::Align::Center);

    // Header row: app title on the left, sound mute toggle on the right.
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.add_css_class("panel-header");

    if let Some(mark) = brand_mark() {
        header.append(&mark);
    }

    let title = gtk::Label::new(Some("vibewatch"));
    title.add_css_class("panel-title");
    title.set_hexpand(true);
    title.set_halign(gtk::Align::Start);
    header.append(&title);

    // Mutes/unmutes sound alerts; state is persisted by `crate::mute` so it
    // survives restarts and is read by the daemon's SoundPlayer per event.
    let mute_btn = gtk::Button::new();
    mute_btn.add_css_class("mute-toggle");
    mute_btn.add_css_class("flat");
    apply_mute_icon(&mute_btn, crate::mute::is_muted());
    let mute_btn_for_click = mute_btn.clone();
    mute_btn.connect_clicked(move |_| {
        let muted = crate::mute::toggle().unwrap_or(false);
        apply_mute_icon(&mute_btn_for_click, muted);
    });
    header.append(&mute_btn);

    main_box.append(&header);

    // Session list
    let session_list = gtk::ListBox::new();
    session_list.set_selection_mode(gtk::SelectionMode::None);
    session_list.add_css_class("session-list");

    let empty_label = gtk::Label::new(Some("No agents running"));
    empty_label.add_css_class("empty-state");
    session_list.set_placeholder(Some(&empty_label));

    // The list scrolls inside the panel instead of growing it: a 15-agent
    // fleet would otherwise make the drawer taller than the screen. Natural
    // height is propagated so a short list still gets a short panel — the
    // ceiling comes from `max_content_height`, recomputed on every rebuild.
    let scroller = gtk::ScrolledWindow::new();
    scroller.add_css_class("session-scroller");
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_propagate_natural_height(true);
    scroller.set_child(Some(&session_list));

    main_box.append(&scroller);

    // The drawer: a Revealer that slides the panel down from the top edge on
    // show and rolls it back up on hide. `transition_duration == 0` (animate
    // off) makes both transitions snap instantly through the same code path.
    let revealer = gtk::Revealer::new();
    revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
    revealer.set_transition_duration(if panel_cfg.animate {
        panel_cfg.animation_ms
    } else {
        0
    });
    revealer.set_reveal_child(false);
    revealer.set_child(Some(&main_box));
    window.set_content(Some(&revealer));

    // Once the collapse animation finishes (child fully hidden) we unmap the
    // surface so it stops consuming compositor resources and input.
    let collapse_win = window.clone();
    revealer.connect_child_revealed_notify(move |rev| {
        if !rev.is_child_revealed() && !rev.reveals_child() {
            collapse_win.set_visible(false);
        }
    });

    // Track whether the pointer is over the panel so auto-close can hold off
    // while the user is reading or aiming for a button.
    let hovered = Rc::new(Cell::new(false));
    let motion = gtk::EventControllerMotion::new();
    let hovered_enter = hovered.clone();
    motion.connect_enter(move |_, _, _| hovered_enter.set(true));
    let hovered_leave = hovered.clone();
    motion.connect_leave(move |_| hovered_leave.set(false));
    main_box.add_controller(motion);

    // Poll registry every 100ms, only rebuild if data changed.
    // Skip polling when window is hidden to avoid unnecessary work.
    let list_ref = session_list;
    let scroller_ref = scroller;
    let header_ref = header;
    // Keep the inner box (not the revealer) for sizing: its natural height is
    // the full panel height regardless of the slide animation's progress, so
    // the window is sized once and the revealer slides within it.
    let content_ref = main_box;
    let win_ref = window.clone();
    let rev_ref = revealer.clone();
    // `None` means "rebuild on next tick" — used when the window was just
    // shown so we always repaint from a fresh registry read.
    let last_fingerprint: Rc<std::cell::RefCell<Option<u64>>> =
        Rc::new(std::cell::RefCell::new(None));
    let was_visible = Rc::new(Cell::new(false));
    let auto_close_delay = Duration::from_millis(panel_cfg.auto_close_ms);
    let max_visible = panel_cfg.max_visible;
    gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
        if !win_ref.is_visible() {
            *last_fingerprint.borrow_mut() = None;
            was_visible.set(false);
            return gtk::glib::ControlFlow::Continue;
        }
        if !was_visible.replace(true) {
            // false -> true transition: (re)start the auto-close clock.
            keep_alive();
        }

        let sessions = registry.all_by_activity();
        let fp = sessions_fingerprint(&sessions);
        let mut prev = last_fingerprint.borrow_mut();
        if *prev != Some(fp) {
            *prev = Some(fp);
            drop(prev);
            // A rebuild throws the rows away, which resets the scroll offset.
            // Restore it afterwards, or reading the bottom of a busy fleet
            // would be impossible — any agent's state change snaps you back
            // to the top. `Adjustment::set_value` clamps for us if the list
            // got shorter in the meantime.
            let offset = scroller_ref.vadjustment().value();
            rebuild_list(&list_ref, &sessions);
            // Cap right here, not in the idle pass below: the pass bails out
            // while the drawer is sliding, and the slide's own sizing callback
            // measures this scroller. Cap it late and the panel opens at full
            // fleet height, then only shrinks at the next data change.
            cap_list_height(&win_ref, &scroller_ref, &list_ref, &header_ref, max_visible);
            // Resize window height to match content
            let win = win_ref.clone();
            let content = content_ref.clone();
            let rev = rev_ref.clone();
            let scroller = scroller_ref.clone();
            gtk::glib::idle_add_local_once(move || {
                // While the drawer is sliding, the tick callback owns sizing —
                // re-pinning to full height here would flash a black strip for
                // one frame. Skip; the next data change resizes once settled.
                if rev.is_child_revealed() != rev.reveals_child() {
                    return;
                }
                scroller.vadjustment().set_value(offset);
                let (_, natural) = content.preferred_size();
                let h = natural.height().min(panel_height_cap(&win)).max(1);
                // set_default_size is the knob that actually shrinks a GTK
                // window below a previous allocation; set_size_request only
                // pins the minimum, which otherwise keeps the surface wide
                // after a transient wide row (e.g. a 3-button approval bar).
                win.set_size_request(PANEL_WIDTH, h);
                win.set_default_size(PANEL_WIDTH, h);
            });
        } else {
            drop(prev);
        }

        // Auto-close: hide once nothing needs attention and the pointer has
        // left the panel for `auto_close_delay`.
        //
        // Only a session awaiting approval holds the drawer open, because only
        // that one cannot proceed without you. A finish used to hold it too, and
        // in use that was wrong: the drawer stayed parked over the work for as
        // long as it took to notice it. The finish is not lost by closing —
        // `just_finished` is not time-based, so the card stays peach and the row
        // keeps its check until the click acknowledges it or that agent picks
        // the work back up. Announcing pops the drawer open again anyway, and
        // `keep_alive` on show gives it a fresh dwell each time.
        if panel_cfg.auto_close {
            let needs_attention = sessions
                .iter()
                .any(|s| s.status == SessionStatus::WaitingApproval);
            if needs_attention || hovered.get() {
                keep_alive();
            } else if alive_elapsed() >= auto_close_delay {
                hide(&win_ref, &rev_ref);
            }
        }
        gtk::glib::ControlFlow::Continue
    });

    // Start hidden — daemon will toggle visibility via IPC
    window.set_visible(false);

    window
}

/// Panel width; the window's height tracks the revealed content.
const PANEL_WIDTH: i32 = 360;

/// Hard ceiling on panel height, as a share of the monitor it sits on. Caps
/// the height even when `panel.max_visible` rows would fit — approval cards
/// are several times as tall as idle ones, so a row count alone can't bound it.
const MAX_SCREEN_FRACTION: f64 = 1.0 / 3.0;

/// Used when no monitor geometry is available yet — the first sizing pass can
/// run before the layer surface is mapped.
const FALLBACK_SCREEN_HEIGHT: i32 = 1080;

/// Never squeeze the list below this, whatever the monitor says.
const MIN_LIST_HEIGHT: i32 = 80;

/// Ceiling on the whole window: a third of the monitor. Applied to every
/// height we hand the compositor, so no code path can produce a panel taller
/// than that — not the first slide, not a row measured before its CSS lands.
fn panel_height_cap(win: &adw::ApplicationWindow) -> i32 {
    ((monitor_height(win) as f64 * MAX_SCREEN_FRACTION) as i32).max(MIN_LIST_HEIGHT)
}

/// Logical height of the monitor the panel currently sits on.
fn monitor_height(win: &adw::ApplicationWindow) -> i32 {
    let Some(display) = gtk::gdk::Display::default() else {
        return FALLBACK_SCREEN_HEIGHT;
    };
    win.surface()
        .and_then(|s| display.monitor_at_surface(&s))
        .or_else(|| display.monitors().item(0).and_downcast())
        .map(|m: gtk::gdk::Monitor| m.geometry().height())
        .filter(|h| *h > 0)
        .unwrap_or(FALLBACK_SCREEN_HEIGHT)
}

/// Cap the scroller so the list shows at most `max_visible` rows, and the
/// panel as a whole stays under `MAX_SCREEN_FRACTION` of the screen. Rows are
/// measured one by one rather than assumed uniform: an idle card is two lines
/// tall, one with an approval bar is three lines plus a button per choice.
fn cap_list_height(
    win: &adw::ApplicationWindow,
    scroller: &gtk::ScrolledWindow,
    list: &gtk::ListBox,
    header: &gtk::Box,
    max_visible: usize,
) {
    let (_, header_h, _, _) = header.measure(gtk::Orientation::Vertical, PANEL_WIDTH);
    let screen_cap = (panel_height_cap(win) - header_h).max(MIN_LIST_HEIGHT);

    let mut rows_h = 0;
    for i in 0..max_visible as i32 {
        let Some(row) = list.row_at_index(i) else { break };
        let (_, nat, _, _) = row.measure(gtk::Orientation::Vertical, PANEL_WIDTH);
        rows_h += nat;
    }

    // An empty list renders the "No agents running" placeholder, whose height
    // `rows_h` knows nothing about — leave it to the screen cap.
    let cap = if rows_h > 0 {
        rows_h.min(screen_cap)
    } else {
        screen_cap
    };
    scroller.set_max_content_height(cap);
}

/// Find the drawer revealer that wraps the panel content.
fn revealer_of(win: &adw::ApplicationWindow) -> Option<gtk::Revealer> {
    win.content().and_then(|c| c.downcast::<gtk::Revealer>().ok())
}

/// Keep the window exactly as tall as the *currently revealed* portion of the
/// drawer for the duration of the slide. A `GtkRevealer` reports its
/// interpolated size while transitioning, so measuring it each frame lets the
/// surface grow/shrink in lockstep with the slide. Without this the window
/// stays pinned at full height and the not-yet-revealed strip renders as an
/// opaque black rectangle (an unpainted layer-shell buffer).
fn sync_size_during_transition(win: &adw::ApplicationWindow, rev: &gtk::Revealer) {
    let rev = rev.clone();
    win.add_tick_callback(move |win, _clock| {
        let (_, nat, _, _) = rev.measure(gtk::Orientation::Vertical, PANEL_WIDTH);
        // Clamped as well as measured: the very first slide runs before the
        // poll loop has ever capped the list, so the raw measure here is the
        // whole fleet's height. The scroller scrolls when it is allocated less
        // than it asked for, so clamping loses nothing.
        let h = nat.min(panel_height_cap(win)).max(1);
        win.set_size_request(PANEL_WIDTH, h);
        win.set_default_size(PANEL_WIDTH, h);
        // Transition is over once the actual reveal state matches the target.
        if rev.is_child_revealed() == rev.reveals_child() {
            gtk::glib::ControlFlow::Break
        } else {
            gtk::glib::ControlFlow::Continue
        }
    });
}

/// Show the panel: map the surface and slide the drawer down.
pub fn show(win: &adw::ApplicationWindow) {
    let Some(rev) = revealer_of(win) else { return };
    // Also covers the already-open case, which the poll loop's visibility
    // edge never sees: a second pop-up has to buy the drawer more time.
    keep_alive();
    win.set_visible(true);
    win.present();
    rev.set_reveal_child(true);
    if rev.transition_duration() > 0 {
        sync_size_during_transition(win, &rev);
    }
}

/// Hide the panel: roll the drawer up; the surface unmaps when the collapse
/// animation completes (or immediately when animations are off).
fn hide(win: &adw::ApplicationWindow, rev: &gtk::Revealer) {
    rev.set_reveal_child(false);
    if rev.transition_duration() == 0 {
        win.set_visible(false);
    } else {
        sync_size_during_transition(win, rev);
    }
}

/// Roll the drawer up because the user picked a session and is on their way
/// to its pane. Separate entry point from the timer-driven [`hide`] so a row
/// click gets out of the way at once, without waiting out `auto_close_ms` —
/// and regardless of `auto_close`, which only governs the timer.
pub fn dismiss(win: &adw::ApplicationWindow) {
    let Some(rev) = revealer_of(win) else { return };
    hide(win, &rev);
}

/// Toggle the panel open/closed with the drawer animation.
pub fn toggle(win: &adw::ApplicationWindow) {
    let Some(rev) = revealer_of(win) else { return };
    let open = win.is_visible() && rev.reveals_child();
    if open {
        hide(win, &rev);
    } else {
        show(win);
    }
}

/// Rebuild the list from scratch with new session data.
fn rebuild_list(list: &gtk::ListBox, sessions: &[crate::session::Session]) {
    while let Some(row) = list.row_at_index(0) {
        list.remove(&row);
    }
    for session in sessions {
        let row = session_row::build_row(session);
        list.append(&row);
    }
}
