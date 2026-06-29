#!/bin/sh
# vibewatch install bootstrap.
# Usage: curl -fsSL https://raw.githubusercontent.com/Moinax/vibewatch/main/install.sh | sh
# Flags after `-s --` are forwarded to `vibewatch install`:
#   curl -fsSL .../install.sh | sh -s -- --no-service
#
# By default it installs the latest released tag (vX.Y.Z). Override with:
#   VIBEWATCH_VERSION=v1.2.3   curl ... | sh   # pin a specific tag
#   VIBEWATCH_VERSION=main     curl ... | sh   # track the development branch
set -eu

REPO=https://github.com/Moinax/vibewatch

if ! command -v cargo >/dev/null 2>&1; then
    echo "vibewatch install.sh: cargo not found. Install Rust first: https://rustup.rs/" >&2
    exit 1
fi

# Resolve which ref to build: an explicit VIBEWATCH_VERSION, else the highest
# semver tag, else fall back to the default branch (e.g. before the first tag).
version="${VIBEWATCH_VERSION:-}"
if [ -z "$version" ]; then
    version=$(git ls-remote --tags --refs "$REPO" 2>/dev/null \
        | awk -F/ '{print $NF}' | grep '^v[0-9]' | sort -V | tail -1 || true)
fi

if [ "$version" = "main" ] || [ -z "$version" ]; then
    [ -z "$version" ] && echo "vibewatch install.sh: no release tag found, building default branch" >&2
    cargo install --git "$REPO" --force
else
    echo "vibewatch install.sh: installing $version" >&2
    cargo install --git "$REPO" --tag "$version" --force
fi

CARGO_BIN="${CARGO_HOME:-$HOME/.cargo}/bin"
exec "$CARGO_BIN/vibewatch" install "$@"
