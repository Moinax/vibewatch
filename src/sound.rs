use std::path::PathBuf;

use crate::config::SoundConfig;

/// Events that can trigger a sound alert.
pub enum SoundEvent {
    ApprovalNeeded,
    Error,
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

    /// Play the sound associated with the given event.
    /// If sound is disabled in config, this is a no-op.
    pub fn play(&self, event: SoundEvent) {
        if !self.config.enabled {
            return;
        }

        let sound_ref = match event {
            SoundEvent::ApprovalNeeded => &self.config.approval_needed,
            SoundEvent::Error => &self.config.error,
        };

        self.play_sound(sound_ref);
    }

    /// Resolve a builtin sound name to a filesystem path.
    ///
    /// Strips the `builtin:` prefix and looks for `{name}.wav` in:
    /// 1. `/usr/share/vibewatch/sounds/{name}.wav`
    /// 2. `CARGO_MANIFEST_DIR/assets/sounds/{name}.wav`
    pub fn resolve_builtin(&self, name: &str) -> Option<PathBuf> {
        let name = name.strip_prefix("builtin:").unwrap_or(name);

        let system_path = PathBuf::from(format!("/usr/share/vibewatch/sounds/{name}.wav"));
        if system_path.exists() {
            return Some(system_path);
        }

        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let dev_path = PathBuf::from(format!("{manifest_dir}/assets/sounds/{name}.wav"));
        if dev_path.exists() {
            return Some(dev_path);
        }

        None
    }

    /// Play a sound from a reference string.
    ///
    /// The reference can be either a `builtin:name` identifier or an absolute path.
    #[cfg(feature = "sound")]
    fn play_sound(&self, sound_ref: &str) {
        let path = if sound_ref.starts_with("builtin:") {
            match self.resolve_builtin(sound_ref) {
                Some(p) => p,
                None => {
                    eprintln!("vibewatch: builtin sound not found: {sound_ref}");
                    return;
                }
            }
        } else {
            PathBuf::from(sound_ref)
        };

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

/// Play a WAV file using rodio.
#[cfg(feature = "sound")]
fn play_file(path: &std::path::Path) -> anyhow::Result<()> {
    use rodio::{Decoder, OutputStream, Sink};
    use std::fs::File;
    use std::io::BufReader;

    let (_stream, stream_handle) = OutputStream::try_default()?;
    let sink = Sink::try_new(&stream_handle)?;

    let file = File::open(path)?;
    let source = Decoder::new(BufReader::new(file))?;
    sink.append(source);
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
            error: "builtin:alert".to_string(),
        };
        let player = SoundPlayer::new(config);
        player.play(SoundEvent::ApprovalNeeded);
        player.play(SoundEvent::Error);
    }

    #[test]
    fn test_resolve_builtin_format() {
        let config = SoundConfig::default();
        let player = SoundPlayer::new(config);
        // Files don't exist yet, so resolve returns None — but must not panic
        let result = player.resolve_builtin("builtin:chime");
        assert!(result.is_none());
        let result = player.resolve_builtin("builtin:success");
        assert!(result.is_none());
        let result = player.resolve_builtin("builtin:alert");
        assert!(result.is_none());
    }
}
