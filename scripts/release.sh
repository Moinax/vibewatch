#!/usr/bin/env bash
# Cut a vibewatch release in one command: bump the version, commit, tag vX.Y.Z,
# push, and publish a GitHub Release with auto-generated notes.
#
# Usage: scripts/release.sh <patch|minor|major|x.y.z>
#
# Prereqs (one-time): cargo install cargo-release && gh auth login
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

# Publish the GitHub Release page from the freshly pushed tag (optional but nice).
if command -v gh >/dev/null 2>&1; then
    gh release create "$tag" --title "$tag" --generate-notes
    echo "Published GitHub Release $tag"
else
    echo "gh not found — tag $tag pushed, but no GitHub Release page created." >&2
    echo "Install gh and run: gh release create $tag --generate-notes" >&2
fi

echo "Released $tag"
