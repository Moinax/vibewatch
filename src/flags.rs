//! Persistent, process-wide toggles the panel header owns: sound mute, and
//! whether events are allowed to pop the drawer open by themselves.
//!
//! Each flag is a tiny file (`1` = on, anything else = off) under the XDG state
//! directory, so it survives restarts and is shared between the panel button
//! (which flips it) and the daemon (which reads it on the — relatively rare —
//! sound or announce event).

use std::path::PathBuf;

/// A named on/off file, with the value it takes before anyone has flipped it.
/// The default travels with the name so a reader and a toggler cannot disagree
/// about what a missing file means.
#[derive(Clone, Copy)]
pub struct Flag {
    name: &'static str,
    default: bool,
}

/// Sound alerts are silenced. Defaults to unmuted.
pub const MUTED: Flag = Flag {
    name: "muted",
    default: false,
};

/// A finish or an approval is allowed to slide the drawer open on its own.
/// Off means the panel only ever opens from a click; waybar keeps reporting.
/// Defaults to on, matching the behaviour before the toggle existed.
pub const AUTO_EXPAND: Flag = Flag {
    name: "auto-expand",
    default: true,
};

impl Flag {
    /// Path to the flag file: `$XDG_STATE_HOME/vibewatch/<name>`
    /// (falls back to `~/.local/state/vibewatch/<name>`).
    fn path(self) -> PathBuf {
        dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .unwrap_or_else(|| PathBuf::from("~/.local/state"))
            .join("vibewatch")
            .join(self.name)
    }

    /// Whether the flag is on. The default stands in when the file is missing
    /// or unreadable; an explicit `0` is not the same as never having been set.
    pub fn is_on(self) -> bool {
        std::fs::read_to_string(self.path())
            .map(|s| s.trim() == "1")
            .unwrap_or(self.default)
    }

    /// Flip the flag and persist it. Returns the state as it now reads back,
    /// not the state we asked for: a write that fails (read-only or full state
    /// dir) must not leave the button's icon claiming a flip that never landed.
    pub fn toggle(self) -> bool {
        let next = !self.is_on();
        let path = self.path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, if next { "1" } else { "0" });
        self.is_on()
    }
}
