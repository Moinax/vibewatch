//! Persistent, process-wide mute state for sound alerts.
//!
//! The flag is backed by a tiny file (`1` = muted, anything else = unmuted)
//! under the XDG state directory, so it survives restarts and is shared
//! between the panel button (which flips it) and the daemon's `SoundPlayer`
//! (which reads it before playing). File I/O is trivial and only happens on a
//! toggle or on a — relatively rare — sound event.

use std::path::PathBuf;

/// Path to the mute state file: `$XDG_STATE_HOME/vibewatch/muted`
/// (falls back to `~/.local/state/vibewatch/muted`).
pub fn state_path() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(|| PathBuf::from("~/.local/state"))
        .join("vibewatch")
        .join("muted")
}

/// Whether sound alerts are currently muted. Defaults to unmuted if the file
/// is missing or unreadable.
pub fn is_muted() -> bool {
    std::fs::read_to_string(state_path())
        .map(|s| s.trim() == "1")
        .unwrap_or(false)
}

/// Persist the mute state, creating the parent directory as needed.
pub fn set_muted(muted: bool) -> std::io::Result<()> {
    let path = state_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, if muted { "1" } else { "0" })
}

/// Flip the current mute state and persist it. Returns the new state.
pub fn toggle() -> std::io::Result<bool> {
    let next = !is_muted();
    set_muted(next)?;
    Ok(next)
}
