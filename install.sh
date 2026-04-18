#!/bin/sh
# vibewatch install bootstrap.
# Usage: curl -fsSL https://raw.githubusercontent.com/Moinax/vibewatch/main/install.sh | sh
# Flags after `-s --` are forwarded to `vibewatch install`:
#   curl -fsSL .../install.sh | sh -s -- --no-service
set -eu

if ! command -v cargo >/dev/null 2>&1; then
    echo "vibewatch install.sh: cargo not found. Install Rust first: https://rustup.rs/" >&2
    exit 1
fi

cargo install --git https://github.com/Moinax/vibewatch

CARGO_BIN="${CARGO_HOME:-$HOME/.cargo}/bin"
exec "$CARGO_BIN/vibewatch" install "$@"
