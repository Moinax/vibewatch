# Sound Assets

Built-in sound files for vibewatch alerts. These are **embedded into the
binary** at compile time (`include_bytes!` in `src/sound.rs`), so they always
resolve regardless of how vibewatch is installed — there is no runtime
filesystem lookup.

## Files

- **chime.wav** — `builtin:chime`, played when the agent needs approval (a question)
- **success.wav** — `builtin:success`, played when the agent finishes and goes idle
- **alert.wav** — `builtin:alert`, a reserved error alert (not auto-triggered)

## Format

Standard WAV (PCM, 16-bit, 44100 Hz mono). Short sounds (0.5–1 s) work best.

## Referencing

In `config.toml`, a sound is either a `builtin:NAME` reference (resolved to the
embedded bytes above) or an absolute path to a WAV on disk:

```toml
[sounds]
approval_needed = "builtin:chime"
idle = "builtin:success"
# or a custom file:
# idle = "/home/user/sounds/done.wav"
```

To change the default sounds, replace the WAV files in this directory and
rebuild.
