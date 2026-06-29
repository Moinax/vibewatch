# Releasing vibewatch

vibewatch uses semver tags (`vX.Y.Z`) as releases. There are no prebuilt
binaries — everything is built from source via `cargo install --git`, so a
release is just **a version bump + a git tag + a GitHub Release page**.

## One-time setup

```sh
cargo install cargo-release   # the version-bump/tag/push driver
gh auth login                 # so the script can create the Release page
```

## Cut a release

```sh
scripts/release.sh patch   # 0.2.0 -> 0.2.1  (bug fixes)
scripts/release.sh minor   # 0.2.0 -> 0.3.0  (new features)
scripts/release.sh major   # 0.2.0 -> 1.0.0  (breaking changes)
```

The script (via `cargo-release` + `release.toml`) bumps `Cargo.toml`, commits
`chore(release): vX.Y.Z`, creates and pushes the `vX.Y.Z` tag, then publishes a
GitHub Release with notes generated from the commits since the last tag. Use
[conventional commit](https://www.conventionalcommits.org/) prefixes
(`fix:`, `feat:`, …) so those notes read well.

## How updates reach machines

`vibewatch --version` reports the embedded version, e.g. `vibewatch 0.2.0 (abc123)`.
`install.sh` installs the **highest semver tag** by default:

```sh
curl -fsSL https://raw.githubusercontent.com/Moinax/vibewatch/main/install.sh | sh
VIBEWATCH_VERSION=v0.2.1 curl -fsSL .../install.sh | sh   # pin a tag
VIBEWATCH_VERSION=main   curl -fsSL .../install.sh | sh   # track main (bleeding)
```

The dotfiles updater (`manage.sh update`) compares the installed version against
the latest tag and only rebuilds when they differ — so changes propagate to
machines **when you cut a release**, not on every push to `main`.
