#!/usr/bin/env bash
# Cut a vibewatch release in one command: bump the version, commit, tag vX.Y.Z,
# and push. The GitHub Release page is published by the `Release` workflow the
# tag triggers, once it has verified the tag and built the binary — publishing
# from here as well would race that job for the page, and would announce the
# release before anything had checked it.
#
# Usage: scripts/release.sh <patch|minor|major|x.y.z>
#
# Prereqs (one-time): cargo install cargo-release
set -euo pipefail

level="${1:-}"
if [ -z "$level" ]; then
    echo "usage: scripts/release.sh <patch|minor|major|x.y.z>" >&2
    exit 1
fi

cd "$(dirname "$0")/.."

if ! command -v cargo-release >/dev/null 2>&1; then
    echo "cargo-release not found — install it: cargo install cargo-release" >&2
    exit 1
fi

# Bump Cargo.toml, commit, tag, and push (settings in release.toml).
cargo release "$level" --execute --no-confirm

version=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
tag="v${version}"

echo "Pushed $tag — the Release workflow now verifies it and publishes the page."
if command -v gh >/dev/null 2>&1; then
    echo "Watch it: gh run watch \$(gh run list --workflow Release --limit 1 --json databaseId --jq '.[0].databaseId')"
fi
