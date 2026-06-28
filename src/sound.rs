use crate::config::SoundConfig;

/// Events that can trigger a sound alert.
pub enum SoundEvent {
    /// The agent is asking for approval/permission (a question).
    ApprovalNeeded,
    /// The agent finished responding and went idle.
    Idle,
    /// The agent hit an error. Reserved — not auto-triggered by the daemon
    /// (tool failures fire constantly during normal work), but kept as a
    /// configurable hook for callers that want it.
    #[allow(dead_code)]
    Error,
}

// Built-in sound assets, embedded into the binary so `builtin:*` references
// always resolve regardless of how vibewatch was installed (no reliance on a
// system data dir or the build directory at runtime).
#[cfg(feature = "sound")]
const BUILTIN_CHIME: &[u8] = include_bytes!("../assets/sounds/chime.wav");
#[cfg(feature = "sound")]
const BUILTIN_SUCCESS: &[u8] = include_bytes!("../assets/sounds/success.wav");
#[cfg(feature = "sound")]
const BUILTIN_ALERT: &[u8] = include_bytes!("../assets/sounds/alert.wav");

/// Map a `builtin:NAME` reference (or bare `NAME`) to its embedded bytes.
#[cfg(feature = "sound")]
fn builtin_bytes(sound_ref: &str) -> Option<&'static [u8]> {
    match sound_ref.strip_prefix("builtin:").unwrap_or(sound_ref) {
        "chime" => Some(BUILTIN_CHIME),
        "success" => Some(BUILTIN_SUCCESS),
        "alert" => Some(BUILTIN_ALERT),
        _ => None,
    }
}

/// Plays sound alerts based on configuration.
pub struct SoundPlayer {
    config: SoundConfig,
}

impl SoundPlayer {
    /// Create a new sound player with the given configuration.
    pub fn new(config: SoundConfig) -> Self {
        Self { config }
    }

    /// Play the sound associated with the given event. No-op when sound is
    /// disabled in config or the user has muted alerts via the panel toggle.
    pub fn play(&self, event: SoundEvent) {
        if !self.config.enabled {
            return;
        }
        if crate::mute::is_muted() {
            return;
        }

        let sound_ref = match event {
            SoundEvent::ApprovalNeeded => &self.config.approval_needed,
            SoundEvent::Idle => &self.config.idle,
            SoundEvent::Error => &self.config.error,
        };

        self.play_sound(sound_ref);
    }

    /// Play a sound from a reference string: either a `builtin:NAME`
    /// identifier (resolved to embedded bytes) or an absolute file path.
    #[cfg(feature = "sound")]
    fn play_sound(&self, sound_ref: &str) {
        if sound_ref.is_empty() {
            return;
        }

        if sound_ref.starts_with("builtin:") {
            match builtin_bytes(sound_ref) {
                Some(bytes) => {
                    std::thread::spawn(move || {
                        if let Err(e) = play_bytes(bytes) {
                            eprintln!("vibewatch: failed to play sound: {e}");
                        }
                    });
                }
                None => eprintln!("vibewatch: unknown builtin sound: {sound_ref}"),
            }
            return;
        }

        let path = std::path::PathBuf::from(sound_ref);
        if !path.exists() {
            eprintln!("vibewatch: sound file not found: {}", path.display());
            return;
        }
        std::thread::spawn(move || {
            if let Err(e) = play_file(&path) {
                eprintln!("vibewatch: failed to play sound: {e}");
            }
        });
    }

    #[cfg(not(feature = "sound"))]
    fn play_sound(&self, _sound_ref: &str) {
        // No-op when sound feature is disabled
    }
}

/// Play a WAV from embedded bytes on the default output device.
#[cfg(feature = "sound")]
fn play_bytes(bytes: &'static [u8]) -> anyhow::Result<()> {
    use rodio::{Decoder, OutputStream, Sink};
    let (_stream, stream_handle) = OutputStream::try_default()?;
    let sink = Sink::try_new(&stream_handle)?;
    sink.append(Decoder::new(std::io::Cursor::new(bytes))?);
    sink.sleep_until_end();
    Ok(())
}

/// Play a WAV file from disk on the default output device.
#[cfg(feature = "sound")]
fn play_file(path: &std::path::Path) -> anyhow::Result<()> {
    use rodio::{Decoder, OutputStream, Sink};
    use std::fs::File;
    use std::io::BufReader;
    let (_stream, stream_handle) = OutputStream::try_default()?;
    let sink = Sink::try_new(&stream_handle)?;
    sink.append(Decoder::new(BufReader::new(File::open(path)?))?);
    sink.sleep_until_end();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disabled_sound_does_not_error() {
        let config = SoundConfig {
            enabled: false,
            approval_needed: "builtin:chime".to_string(),
            idle: "builtin:success".to_string(),
            error: "builtin:alert".to_string(),
        };
        let player = SoundPlayer::new(config);
        player.play(SoundEvent::ApprovalNeeded);
        player.play(SoundEvent::Idle);
        player.play(SoundEvent::Error);
    }

    #[cfg(feature = "sound")]
    #[test]
    fn test_builtin_bytes_resolve() {
        assert!(builtin_bytes("builtin:chime").is_some());
        assert!(builtin_bytes("builtin:success").is_some());
        assert!(builtin_bytes("builtin:alert").is_some());
        assert!(builtin_bytes("builtin:nope").is_none());
        // Bare names (no prefix) resolve too.
        assert!(builtin_bytes("chime").is_some());
    }
}
