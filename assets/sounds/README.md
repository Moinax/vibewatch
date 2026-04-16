# Sound Assets

This directory contains the built-in sound files for vibewatch alerts.

## Expected WAV Files

Place the following WAV files in this directory:

- **chime.wav** - Played when an AI agent needs user approval (e.g., a tool-use prompt)
- **success.wav** - Played when an AI agent task completes successfully
- **alert.wav** - Played when an AI agent encounters an error

## Format

Files should be standard WAV format (PCM, 16-bit, 44100 Hz recommended).
Short sounds (0.5-2 seconds) work best for notifications.

## Resolution Order

The sound player looks for built-in sounds in this order:

1. `/usr/share/vibewatch/sounds/{name}.wav` (system install)
2. `{CARGO_MANIFEST_DIR}/assets/sounds/{name}.wav` (development)
