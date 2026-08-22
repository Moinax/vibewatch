//! Account rate limits, gathered from the agents' own accounts and cached here.
//!
//! Each provider hands these out somewhere different, and neither place is a
//! transcript — an agent's session file records what it spent in tokens, never
//! what the account has left:
//!
//! * **Claude** answers `GET /api/oauth/usage` with the OAuth token Claude Code
//!   already keeps in `~/.claude/.credentials.json`. Nothing on disk carries
//!   the figure, so this is the only fresh source there is.
//! * **Codex** writes its whole snapshot beside every `token_count` line in
//!   `~/.codex/sessions/**/*.jsonl`, so the newest one survives on disk with no
//!   network at all.
//!
//! Both fold into one cache of our own under the state dir, which is what every
//! surface reads. That keeps the read path off the network — the panel polls at
//! 10 Hz — and means a fetch that fails leaves the last known figures standing
//! rather than blanking the section. They then age visibly, which is the honest
//! outcome: these numbers are stale far more often than they are missing.
//!
//! Times are Unix seconds throughout. Codex reports them that way already, and
//! it spares every reader a date parse — the one place an ISO-8601 string
//! arrives is Claude's response, converted once on the way in.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::session::AgentKind;

/// Claude's usage endpoint, and the beta header its OAuth tokens are scoped by.
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const USAGE_BETA: &str = "oauth-2025-04-20";

/// A hung request must not hold a refresh open: the caller is a hook handler or
/// a panel open, both of which have something better to do.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Floor between two refreshes.
///
/// Refreshes are event-driven — a turn ending, the panel opening — and a busy
/// fleet fires those far faster than a quota moves. Five minutes is well inside
/// the resolution anyone reads these at, and keeps a fifteen-agent fleet from
/// turning every finished turn into a request.
const MIN_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// How far back a Codex rollout is worth opening, and how many to open.
///
/// Newest-first, so the first file almost always answers. The margin covers
/// runs of sessions that never reached a token count and so never recorded a
/// snapshot; past it, the data is genuinely absent rather than further away.
const CODEX_SCAN_DAYS: u64 = 14;
const CODEX_MAX_FILES: usize = 32;

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

/// One rolling window of a provider's quota.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Window {
    /// Stable identifier: `five_hour`, `seven_day`, or a model slug.
    pub id: String,
    /// Short display label — "5h", "Week", "Fable".
    pub label: String,
    /// Share of the window consumed, 0-100.
    pub used_percent: f64,
    /// Unix seconds at which the window resets. `None` until the window has
    /// traffic: providers report an untouched one with no clock on it.
    #[serde(default)]
    pub resets_at: Option<i64>,
}

/// One provider's quota, as of its last report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// `claude` or `codex`.
    pub provider: String,
    /// Rendered in this order. A window a provider adds or brings back (Codex's
    /// paused 5-hour, a new model weekly) arrives here and is painted with no
    /// change anywhere downstream.
    pub windows: Vec<Window>,
    /// Unix seconds at which the provider reported these numbers — not when we
    /// read them. Claude's can lag by hours; readers show the age rather than
    /// pretend the figures are current.
    pub as_of: i64,
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

/// Where the merged snapshots live between refreshes.
pub fn cache_path() -> PathBuf {
    crate::config::state_dir().join("account-limits.json")
}

/// Every provider's latest snapshot. Cheap enough for a 10 Hz poll: one open
/// and a sub-kilobyte parse.
pub fn read() -> Vec<Snapshot> {
    std::fs::read_to_string(cache_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Replace the cache. Written beside and renamed, because the panel reads this
/// file ten times a second and a torn read would blank the section for a frame.
fn write_cache(snapshots: &[Snapshot]) {
    let path = cache_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(serialized) = serde_json::to_string(snapshots) else {
        return;
    };
    // Stamped with the pid: the single-flight guard below keeps this process
    // to one writer, but two daemons sharing a state dir would otherwise
    // interleave into the same temp file and rename torn JSON over the cache.
    let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    if std::fs::write(&temp, serialized).is_ok() {
        let _ = std::fs::rename(&temp, &path);
    }
}

/// How long ago the cache was last written, or `None` if it never was.
fn cache_age() -> Option<std::time::Duration> {
    std::fs::metadata(cache_path())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
}

// ---------------------------------------------------------------------------
// Refresh
// ---------------------------------------------------------------------------

/// Set while a refresh is running, so only one is.
static REFRESH_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Clears [`REFRESH_IN_FLIGHT`] however the refresh ends — a panic in the
/// blocking task would otherwise leave the flag set and every later refresh
/// silently skipped for the life of the daemon.
struct InFlight;

impl Drop for InFlight {
    fn drop(&mut self) {
        REFRESH_IN_FLIGHT.store(false, Ordering::SeqCst);
    }
}

/// Re-read both providers off the runtime, unless the cache is recent enough
/// or a refresh is already running.
///
/// Both gates are checked here, on the caller's thread, and deliberately not
/// inside the spawned task. The cache's mtime is only bumped once a refresh
/// *finishes*, so a floor read after the spawn lets through every event that
/// lands while a fetch is in flight — fifteen agents ending a turn together
/// would each open their own request, against a floor written to prevent
/// exactly that. The flag closes the remaining window, which the floor cannot:
/// two events in the same millisecond read the same mtime.
pub fn refresh_in_background(enabled: bool) {
    if !enabled {
        return;
    }
    if cache_age().is_some_and(|age| age < MIN_REFRESH_INTERVAL) {
        return;
    }
    if REFRESH_IN_FLIGHT.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::task::spawn_blocking(|| {
        let _guard = InFlight;
        refresh();
    });
}

/// Re-read both providers and replace the cache.
///
/// A provider that cannot be reached keeps whatever it last reported, so one
/// leg failing never costs the other's figures — nor its own previous ones.
pub fn refresh() {
    let merged = merge(read(), [fetch_claude(), read_codex()]);
    write_cache(&merged);
}

/// Fold whatever answered into whatever was cached, one entry per provider.
///
/// A provider that did not answer keeps its previous entry untouched. That is
/// the whole failure policy: one leg going quiet — no network, an expired
/// token, Codex never run on this machine — costs neither the other leg's
/// figures nor its own last ones, which then simply age in view.
fn merge(
    mut cached: Vec<Snapshot>,
    fresh: impl IntoIterator<Item = Option<Snapshot>>,
) -> Vec<Snapshot> {
    for snapshot in fresh.into_iter().flatten() {
        match cached.iter_mut().find(|s| s.provider == snapshot.provider) {
            Some(current) => *current = snapshot,
            None => cached.push(snapshot),
        }
    }
    cached
}

// ---------------------------------------------------------------------------
// Claude
// ---------------------------------------------------------------------------

/// The OAuth access token Claude Code holds for the subscription account.
///
/// Read on each refresh rather than held: Claude Code rotates it, and a copy
/// kept in memory would go stale and start answering 401 with no way back.
fn claude_token() -> Option<String> {
    let path = dirs::home_dir()?.join(".claude").join(".credentials.json");
    token_from_credentials(&std::fs::read_to_string(path).ok()?)
}

/// The access token in Claude Code's credentials file.
///
/// Deliberately narrow: it reaches for one field and returns `None` for
/// everything else, so a file that also holds the MCP servers' tokens cannot
/// have one of those picked up by a looser search.
fn token_from_credentials(raw: &str) -> Option<String> {
    let value: Value = serde_json::from_str(raw).ok()?;
    Some(
        value
            .get("claudeAiOauth")?
            .get("accessToken")?
            .as_str()?
            .to_string(),
    )
}

/// Claude's quota, straight from the account.
///
/// `None` on anything at all going wrong — no token, no network, a refusal, a
/// shape we do not recognise. The caller keeps the previous snapshot in every
/// one of those cases, which is why none of them are worth telling apart here.
fn fetch_claude() -> Option<Snapshot> {
    let token = claude_token()?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .build()
        .into();
    let body: Value = agent
        .get(USAGE_URL)
        .header("Authorization", &format!("Bearer {token}"))
        .header("anthropic-beta", USAGE_BETA)
        .call()
        .ok()?
        .into_body()
        .read_json()
        .ok()?;
    claude_snapshot(&body, now_epoch())
}

/// Fold Claude's response into the contract.
///
/// Read tolerantly, by name rather than by shape: the response has already
/// gained a `limits` array beside the older per-bucket fields, and the next
/// window to appear must not take the whole snapshot down with it. Each window
/// is looked for in both spellings, newest first.
fn claude_snapshot(body: &Value, now: i64) -> Option<Snapshot> {
    let limits = body.get("limits").and_then(Value::as_array);
    let mut windows = Vec::new();

    for (bucket, kind, id, label) in [
        ("five_hour", "session", "five_hour", "5h"),
        ("seven_day", "weekly_all", "seven_day", "Week"),
    ] {
        let scoped = limits.and_then(|l| {
            l.iter()
                .find(|limit| limit.get("kind").and_then(Value::as_str) == Some(kind))
        });
        if let Some(window) = window_from(
            id,
            label,
            body.get(bucket)
                .and_then(|b| b.get("utilization"))
                .and_then(Value::as_f64)
                .or_else(|| scoped?.get("percent")?.as_f64()),
            body.get(bucket)
                .and_then(|b| b.get("resets_at"))
                .or_else(|| scoped?.get("resets_at")),
        ) {
            windows.push(window);
        }
    }

    // Per-model weeklies: whatever the account has, named by the provider. The
    // set moves with the model line-up, so nothing here enumerates them.
    for limit in limits.into_iter().flatten() {
        if limit.get("kind").and_then(Value::as_str) != Some("weekly_scoped") {
            continue;
        }
        let Some(name) = limit
            .pointer("/scope/model/display_name")
            .and_then(Value::as_str)
        else {
            continue;
        };
        if let Some(window) = window_from(
            &name.to_lowercase(),
            name,
            limit.get("percent").and_then(Value::as_f64),
            limit.get("resets_at"),
        ) {
            windows.push(window);
        }
    }

    // No windows at all is an account rate limits do not apply to (an API key,
    // Bedrock, Vertex). Nothing to show, and nothing worth clearing a previous
    // snapshot over.
    (!windows.is_empty()).then(|| Snapshot {
        provider: AgentKind::ClaudeCode.slug().to_string(),
        windows,
        as_of: now,
    })
}

/// One window, unless the provider is reporting an untouched placeholder: zero
/// spent and no clock is what an inactive window looks like, and painting it as
/// a real "0% used" would claim knowledge we do not have.
fn window_from(
    id: &str,
    label: &str,
    percent: Option<f64>,
    resets_at: Option<&Value>,
) -> Option<Window> {
    let used_percent = percent?;
    let resets_at = resets_at.and_then(|value| match value {
        Value::String(iso) => parse_iso_epoch(iso),
        other => other.as_i64(),
    });
    if used_percent == 0.0 && resets_at.is_none() {
        return None;
    }
    Some(Window {
        id: id.to_string(),
        label: label.to_string(),
        used_percent,
        resets_at,
    })
}

// ---------------------------------------------------------------------------
// Codex
// ---------------------------------------------------------------------------

/// Codex's quota, recovered from its own session files.
///
/// Codex records its whole rate-limit snapshot beside every token count, so the
/// newest one is on disk whether or not Codex is running — no token and no
/// request on this leg.
fn read_codex() -> Option<Snapshot> {
    let root = crate::codex_rollout::sessions_root()?;
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(CODEX_SCAN_DAYS * 24 * 3600))?;

    let mut files = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // `file_type` rides along with the directory read on Linux, where
            // `path.is_dir()` is a second stat for every entry in a tree that
            // only grows.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(entry.path());
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
                continue;
            };
            if modified >= cutoff {
                files.push((modified, path));
            }
        }
    }
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));

    // A file's records can never be newer than the file, so once one has
    // answered there is no point opening anything modified before it. In
    // practice that stops on the first or second.
    let mut best: Option<Snapshot> = None;
    for (modified, path) in files.into_iter().take(CODEX_MAX_FILES) {
        // Every failure below skips the file. A `?` here would abandon the
        // whole scan — including a snapshot an earlier file had already
        // yielded — because one rollout was rotated away between the walk and
        // the open, which Codex does while the daemon is refreshing.
        let Ok(since_epoch) = modified.duration_since(std::time::UNIX_EPOCH) else {
            continue;
        };
        let mtime = since_epoch.as_secs() as i64;
        if best.as_ref().is_some_and(|found| found.as_of >= mtime) {
            break;
        }
        let Some(content) = crate::transcript::head_and_tail(&path) else {
            continue;
        };
        if let Some(snapshot) = codex_snapshot(&content, mtime) {
            best = Some(snapshot);
        }
    }
    best
}

/// The last rate-limit snapshot in a rollout's tail.
///
/// `fallback_as_of` stands in only for a record that carries no clock of its
/// own — the file's mtime, which is the moment the session last said anything
/// at all rather than the moment it was last metered.
fn codex_snapshot(content: &str, fallback_as_of: i64) -> Option<Snapshot> {
    for line in content.lines().rev() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(limits) = find_rate_limits(&value) else {
            continue;
        };
        let mut windows = Vec::new();
        // `primary` and `secondary` are positions, not windows: which one holds
        // the weekly moves when OpenAI turns the 5-hour back on. The window
        // length is what actually names them.
        for slot in ["primary", "secondary"] {
            let Some(meter) = limits.get(slot).filter(|v| !v.is_null()) else {
                continue;
            };
            let Some(used_percent) = meter.get("used_percent").and_then(Value::as_f64) else {
                continue;
            };
            let (id, label) = match meter.get("window_minutes").and_then(Value::as_i64) {
                Some(300) => ("five_hour", "5h"),
                Some(10080) => ("seven_day", "Week"),
                _ => (
                    slot,
                    if slot == "primary" {
                        "Primary"
                    } else {
                        "Secondary"
                    },
                ),
            };
            windows.push(Window {
                id: id.to_string(),
                label: label.to_string(),
                used_percent,
                resets_at: meter.get("resets_at").and_then(Value::as_i64),
            });
        }
        if !windows.is_empty() {
            // The record's own clock. A session that keeps appending messages
            // after its last token count would otherwise have a meter hours
            // old dated to the minute the file was last touched, and so be
            // painted as current.
            let as_of = value
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_iso_epoch)
                .unwrap_or(fallback_as_of);
            return Some(Snapshot {
                provider: AgentKind::Codex.slug().to_string(),
                windows,
                as_of,
            });
        }
    }
    None
}

/// `rate_limits`, wherever in the record Codex hung it this release.
fn find_rate_limits(value: &Value) -> Option<&Value> {
    if let Some(found) = value.get("rate_limits") {
        return Some(found);
    }
    match value {
        Value::Object(map) => map.values().find_map(find_rate_limits),
        Value::Array(items) => items.iter().find_map(find_rate_limits),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

/// Unix seconds now.
fn now_epoch() -> i64 {
    crate::session::now_epoch() as i64
}

/// Unix seconds for an ISO-8601 instant — the one shape Claude's response uses,
/// and the only date parsing in the crate.
///
/// By hand rather than by crate: this reads `YYYY-MM-DDTHH:MM:SS` plus an
/// optional fraction and offset, which is all the endpoint emits, and a date
/// library would be a dependency bought for one field. Anything it does not
/// recognise is `None`, and the window simply has no clock.
pub fn parse_iso_epoch(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[13] != b':' {
        return None;
    }
    let field = |from: usize, to: usize| text.get(from..to)?.parse::<i64>().ok();
    let (year, month, day) = (field(0, 4)?, field(5, 7)?, field(8, 10)?);
    let (hour, minute, second) = (field(11, 13)?, field(14, 16)?, field(17, 19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let utc = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;

    // Whatever follows the seconds: an optional fraction, then `Z` or an
    // offset. The fraction is dropped — nothing here is worth sub-second
    // resolution.
    let rest = text[19..].trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = match rest.as_bytes().first() {
        None | Some(b'Z') | Some(b'z') => 0,
        Some(sign @ (b'+' | b'-')) => {
            let hours: i64 = rest.get(1..3)?.parse().ok()?;
            // Extended (`+02:30`) is what the endpoint sends; basic (`+0230`)
            // is the same offset spelled without the colon. Reading the
            // minutes from a fixed slice would silently take the basic form's
            // as absent and drop half an hour on the floor.
            let minutes: i64 = match rest.get(3..4) {
                None => 0,
                Some(":") => rest.get(4..6)?.parse().ok()?,
                Some(_) => rest.get(3..5)?.parse().ok()?,
            };
            if !(0..60).contains(&minutes) || hours > 23 {
                return None;
            }
            let magnitude = hours * 3600 + minutes * 60;
            if *sign == b'-' {
                -magnitude
            } else {
                magnitude
            }
        }
        _ => return None,
    };
    Some(utc - offset)
}

/// Days between the epoch and a civil date, proleptic Gregorian.
///
/// Howard Hinnant's `days_from_civil`, which is the standard way to do this
/// without a calendar library and is correct for every year the endpoint can
/// name. March-based, hence the shift: it puts the leap day at the end of the
/// year, where it needs no special case.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_instants_land_on_the_epoch_the_shell_agrees_with() {
        // Cross-checked against `date -d '...' +%s`.
        assert_eq!(parse_iso_epoch("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_iso_epoch("2026-08-22T13:41:41.879Z"),
            Some(1787406101)
        );
        // The other spelling the response carries, same instant either way.
        assert_eq!(
            parse_iso_epoch("2026-08-22T17:29:59.643021+00:00"),
            parse_iso_epoch("2026-08-22T17:29:59Z")
        );
        // A real offset has to move the instant, and in the right direction.
        assert_eq!(
            parse_iso_epoch("2026-08-22T15:41:41+02:00"),
            parse_iso_epoch("2026-08-22T13:41:41Z")
        );
        assert_eq!(
            parse_iso_epoch("2026-08-22T11:41:41-02:00"),
            parse_iso_epoch("2026-08-22T13:41:41Z")
        );
        // Basic format, same offset without the colon. Reading the minutes off
        // a fixed slice took these as absent and silently dropped them.
        assert_eq!(
            parse_iso_epoch("2026-08-22T15:41:41+0200"),
            parse_iso_epoch("2026-08-22T13:41:41Z")
        );
        assert_eq!(
            parse_iso_epoch("2026-08-22T16:11:41+0230"),
            parse_iso_epoch("2026-08-22T13:41:41Z")
        );
        assert_eq!(parse_iso_epoch("2026-08-22T13:41:41+02:99"), None);
        // Leap days, which is where a hand-rolled calendar goes wrong. 2024 has
        // one; 2100 does not, being a century that is not a fourth one — so the
        // day after 28 February 2100 is 1 March, not 29 February.
        assert_eq!(parse_iso_epoch("2024-02-29T00:00:00Z"), Some(1709164800));
        assert_eq!(parse_iso_epoch("2100-02-28T00:00:00Z"), Some(4107456000));
        assert_eq!(parse_iso_epoch("2100-03-01T00:00:00Z"), Some(4107542400));
        assert_eq!(
            parse_iso_epoch("2100-03-01T00:00:00Z").unwrap()
                - parse_iso_epoch("2100-02-28T00:00:00Z").unwrap(),
            86_400
        );
        assert_eq!(parse_iso_epoch("not a date"), None);
        assert_eq!(parse_iso_epoch("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn claudes_response_folds_into_windows() {
        let body: Value = serde_json::from_str(
            r#"{
              "five_hour": {"utilization": 12, "resets_at": "2026-08-22T17:29:59.643021+00:00"},
              "seven_day": {"utilization": 85, "resets_at": "2026-08-24T13:59:59.871777+00:00"},
              "limits": [
                {"kind": "session", "percent": 12, "resets_at": "2026-08-22T17:29:59Z"},
                {"kind": "weekly_all", "percent": 85, "resets_at": "2026-08-24T13:59:59Z"},
                {"kind": "weekly_scoped", "percent": 18, "resets_at": "2026-08-24T13:59:59Z",
                 "scope": {"model": {"display_name": "Fable"}}}
              ]
            }"#,
        )
        .unwrap();
        let snapshot = claude_snapshot(&body, 1_787_406_101).expect("a snapshot");
        let labels: Vec<&str> = snapshot.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["5h", "Week", "Fable"]);
        assert_eq!(snapshot.windows[1].used_percent, 85.0);
        assert_eq!(
            snapshot.windows[0].resets_at,
            parse_iso_epoch("2026-08-22T17:29:59Z")
        );
    }

    #[test]
    fn a_response_carrying_only_the_limits_array_still_folds() {
        // The per-bucket fields are the older spelling; nothing may depend on
        // them still being there.
        let body: Value = serde_json::from_str(
            r#"{"limits": [
                {"kind": "session", "percent": 7, "resets_at": "2026-08-22T17:29:59Z"},
                {"kind": "weekly_all", "percent": 40, "resets_at": "2026-08-24T13:59:59Z"}
            ]}"#,
        )
        .unwrap();
        let snapshot = claude_snapshot(&body, 0).expect("a snapshot");
        assert_eq!(snapshot.windows.len(), 2);
        assert_eq!(snapshot.windows[0].used_percent, 7.0);
    }

    #[test]
    fn an_untouched_window_is_left_out_rather_than_painted_as_zero() {
        let body: Value = serde_json::from_str(
            r#"{"limits": [
                {"kind": "session", "percent": 0, "resets_at": null},
                {"kind": "weekly_all", "percent": 40, "resets_at": "2026-08-24T13:59:59Z"}
            ]}"#,
        )
        .unwrap();
        let snapshot = claude_snapshot(&body, 0).expect("a snapshot");
        let labels: Vec<&str> = snapshot.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["Week"]);
    }

    #[test]
    fn an_account_without_rate_limits_yields_nothing_to_overwrite_with() {
        let body: Value = serde_json::from_str(r#"{"limits": []}"#).unwrap();
        assert_eq!(claude_snapshot(&body, 0), None);
    }

    #[test]
    fn codex_windows_are_named_by_their_length_not_their_slot() {
        // A real rollout line, trimmed to the record that carries the meter.
        let line = r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"limit_id":"codex","primary":{"used_percent":2.0,"window_minutes":10080,"resets_at":1787207448},"secondary":null,"plan_type":"prolite"}}}"#;
        let snapshot = codex_snapshot(line, 1_787_200_000).expect("a snapshot");
        assert_eq!(snapshot.provider, "codex");
        assert_eq!(snapshot.windows.len(), 1);
        assert_eq!(snapshot.windows[0].label, "Week");
        assert_eq!(snapshot.windows[0].id, "seven_day");
        assert_eq!(snapshot.windows[0].used_percent, 2.0);
        assert_eq!(snapshot.windows[0].resets_at, Some(1787207448));
    }

    #[test]
    fn codex_reads_the_last_meter_in_the_file_not_the_first() {
        let content = [
            r#"{"payload":{"rate_limits":{"primary":{"used_percent":1.0,"window_minutes":10080,"resets_at":1}}}}"#,
            r#"{"payload":{"type":"agent_message"}}"#,
            r#"{"payload":{"rate_limits":{"primary":{"used_percent":9.0,"window_minutes":10080,"resets_at":2}}}}"#,
        ]
        .join("\n");
        let snapshot = codex_snapshot(&content, 0).expect("a snapshot");
        assert_eq!(snapshot.windows[0].used_percent, 9.0);
    }

    #[test]
    fn codex_five_hour_window_reappearing_needs_no_change_here() {
        let line = r#"{"payload":{"rate_limits":{"primary":{"used_percent":30.0,"window_minutes":300,"resets_at":5},"secondary":{"used_percent":4.0,"window_minutes":10080,"resets_at":6}}}}"#;
        let snapshot = codex_snapshot(line, 0).expect("a snapshot");
        let labels: Vec<&str> = snapshot.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["5h", "Week"]);
    }

    #[test]
    fn a_quiet_leg_costs_neither_the_other_nor_its_own_last_figures() {
        let cached = vec![
            Snapshot {
                provider: "claude".to_string(),
                windows: vec![Window {
                    id: "five_hour".to_string(),
                    label: "5h".to_string(),
                    used_percent: 9.0,
                    resets_at: Some(10),
                }],
                as_of: 100,
            },
            Snapshot {
                provider: "codex".to_string(),
                windows: Vec::new(),
                as_of: 50,
            },
        ];
        let fresh = Snapshot {
            provider: "claude".to_string(),
            windows: Vec::new(),
            as_of: 200,
        };

        // Claude answered, Codex did not.
        let merged = merge(cached.clone(), [Some(fresh), None]);
        assert_eq!(
            merged.len(),
            2,
            "one entry per provider, not one per answer"
        );
        assert_eq!(merged[0].as_of, 200, "the answer replaces the cached entry");
        assert_eq!(merged[1].as_of, 50, "the quiet leg keeps what it had");

        // Nobody answered: the cache is returned untouched rather than emptied.
        assert_eq!(merge(cached.clone(), [None, None]), cached);
    }

    #[test]
    fn a_provider_seen_for_the_first_time_is_added_not_dropped() {
        let fresh = Snapshot {
            provider: "codex".to_string(),
            windows: Vec::new(),
            as_of: 7,
        };
        let merged = merge(Vec::new(), [Some(fresh)]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].provider, "codex");
    }

    #[test]
    fn only_the_subscription_token_is_read_out_of_the_credentials() {
        // Shaped like the real file, which also holds one token per MCP server.
        let raw = r#"{
          "mcpOAuth": {"linear|abc": {"accessToken": "mcp-token", "serverName": "linear"}},
          "claudeAiOauth": {"accessToken": "subscription-token", "subscriptionType": "max"}
        }"#;
        assert_eq!(
            token_from_credentials(raw).as_deref(),
            Some("subscription-token")
        );
        // An MCP-only file must not have one of those picked up instead.
        let mcp_only = r#"{"mcpOAuth": {"linear|abc": {"accessToken": "mcp-token"}}}"#;
        assert_eq!(token_from_credentials(mcp_only), None);
        assert_eq!(token_from_credentials("{}"), None);
        assert_eq!(token_from_credentials("not json"), None);
    }

    #[test]
    fn the_meter_is_dated_by_its_own_record_when_it_carries_one() {
        let line = r#"{"timestamp":"2026-08-20T00:04:09.360Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":2.0,"window_minutes":10080,"resets_at":1787207448}}}}"#;
        let snapshot = codex_snapshot(line, 9_999_999_999).expect("a snapshot");
        assert_eq!(
            snapshot.as_of,
            parse_iso_epoch("2026-08-20T00:04:09.360Z").expect("parses"),
            "the record's clock, not the file's mtime"
        );

        // Only a record with no clock of its own falls back to the file.
        let undated = r#"{"payload":{"rate_limits":{"primary":{"used_percent":2.0,"window_minutes":10080,"resets_at":1}}}}"#;
        assert_eq!(
            codex_snapshot(undated, 4242).expect("a snapshot").as_of,
            4242
        );
    }

    #[test]
    fn a_rollout_with_no_meter_yields_nothing() {
        assert_eq!(
            codex_snapshot(r#"{"payload":{"type":"agent_message"}}"#, 0),
            None
        );
    }
}
