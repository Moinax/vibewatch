use std::env;
use std::process::Command;

/// The short commit `vibewatch --version` reports.
fn git_short_sha() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

/// A commit handed to us instead of read from a repository, trimmed to git's own
/// short length. `char`-wise rather than by byte, since this is outside input.
fn env_sha(key: &str) -> Option<String> {
    let raw = env::var(key).ok()?;
    let sha: String = raw.trim().chars().take(8).collect();
    (!sha.is_empty()).then_some(sha)
}

fn main() {
    // A repository is the best source but not always a source at all: a release
    // tarball carries no `.git`, and `actions/checkout` silently falls back to a
    // REST API download when the runner has no git — v0.7.3 and v0.7.4 both
    // shipped stamped `unknown` that way, because this stamp degrades instead of
    // failing. So the environment gets a say: `VIBEWATCH_GIT_SHA` for a builder
    // that knows what it is building, `GITHUB_SHA` because Actions exports it
    // for free.
    let sha = env_sha("VIBEWATCH_GIT_SHA")
        .or_else(git_short_sha)
        .or_else(|| env_sha("GITHUB_SHA"))
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=VIBEWATCH_GIT_SHA={sha}");
    println!("cargo:rerun-if-env-changed=VIBEWATCH_GIT_SHA");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
    println!("cargo:rerun-if-changed=.git/packed-refs");
}
