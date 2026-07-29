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
        s.pending_approval
            .as_ref()
            .map(|p| (&p.request_id, p.choices.len()))
            .hash(&mut h);
    }
    h.finish()
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
    let load_palette = |provider: &gtk::CssProvider, dark: bool| {
        provider.load_from_string(if dark { PALETTE_MOCHA } else { PALETTE_LATTE });
    };

    let style_manager = adw::StyleManager::default();
    load_palette(&palette_provider, style_manager.is_dark());
    let palette_for_notify = palette_provider.clone();
    style_manager.connect_dark_notify(move |sm| {
        load_palette(&palette_for_notify, sm.is_dark());
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
    // Timestamp the panel became visible / was last "kept alive" (hovered or
    // attention-needing). Auto-close fires once this is older than the delay.
    let alive_since = Rc::new(Cell::new(Instant::now()));
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
            alive_since.set(Instant::now());
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
        // left the panel for `auto_close_delay`. Sessions awaiting approval
        // keep it open indefinitely.
        if panel_cfg.auto_close {
            let needs_attention = sessions
                .iter()
                .any(|s| s.status == SessionStatus::WaitingApproval);
            if needs_attention || hovered.get() {
                alive_since.set(Instant::now());
            } else if alive_since.get().elapsed() >= auto_close_delay {
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
