//! T3 Code integration.
//!
//! T3 Code runs its agents headlessly: the desktop app's server process spawns
//! one `claude`/`codex` per thread and speaks to it over stdio, so the agent has
//! no terminal of its own and its output never reaches a pane. Two things follow
//! from that, and this module exists for both.
//!
//! The first is that such a process looks exactly like a scripted one — it is
//! launched with `--output-format stream-json`, which [`is_programmatic_pid`]
//! reads as "not a session someone is sitting in front of" and filters out. That
//! verdict is right for a `claude -p` in a shell script and wrong here: a T3
//! thread is a session someone is watching, in a window they can be sent to. The
//! mark that separates the two is [`hosted_by`].
//!
//! The second is that the agent's own account of itself is not the one the user
//! sees. They named the thread in T3, or T3 titled it for them, and that title —
//! along with the thread id needed to point at it, and T3's own record of
//! whether it is blocked on the user — lives in the app's state database, not in
//! the agent's transcript. [`threads`] reads it.
//!
//! Everything here degrades to nothing when T3 Code is not installed or not
//! running, which is the common case.

use std::path::{Path, PathBuf};

use crate::session::Ask;

/// A running T3 Code server, as it announces itself on disk.
///
/// T3 keeps one state directory per build channel — `userdata` for the release
/// app, `dev` for a local one — and both can be running at once, so this is a
/// list everywhere rather than a single value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runtime {
    /// The server process that spawns this profile's agents.
    pub pid: u32,
    /// The profile's state directory, e.g. `~/.t3/userdata`.
    pub base_dir: PathBuf,
}

/// The T3 state directories to look in, most-likely first.
const PROFILES: [&str; 2] = ["userdata", "dev"];

/// Every T3 Code server currently running, read from the `server-runtime.json`
/// each one writes on startup.
///
/// The file outlives the process that wrote it — it is not cleaned up on exit,
/// and a crashed server leaves its own behind — so the pid it names is checked
/// against a live process that still looks like T3. Without that check a stale
/// file whose pid has since been recycled would make an unrelated process's
/// children look like T3 threads.
pub fn live_runtimes() -> Vec<Runtime> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    PROFILES
        .iter()
        .filter_map(|profile| {
            let base_dir = home.join(".t3").join(profile);
            let pid = runtime_pid(&base_dir)?;
            looks_like_t3_server(pid).then_some(Runtime { pid, base_dir })
        })
        .collect()
}

/// The server pid recorded in a profile's `server-runtime.json`, if it has one.
fn runtime_pid(base_dir: &Path) -> Option<u32> {
    let raw = std::fs::read_to_string(base_dir.join("server-runtime.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    u32::try_from(value.get("pid")?.as_u64()?).ok()
}

/// Is `pid` a live process that could be a T3 Code server?
///
/// The name test is what makes this more than a liveness check: pids are
/// recycled, and the recorded one is only as fresh as the last server start.
/// Every way T3 runs its server puts the string in the command line — `t3code`
/// for the packaged app, the checkout path for a development build. Both
/// spellings, since the checkout may be capitalised; matching a two-character
/// string is not worth a lowercased copy of an Electron process's argv.
fn looks_like_t3_server(pid: u32) -> bool {
    crate::session::proc_cmdline(pid).is_some_and(|raw| raw.contains("t3") || raw.contains("T3"))
}

/// The T3 server hosting `pid`, if one is.
///
/// The test is deliberately parenthood and not ancestry. T3 spawns each thread's
/// agent as a direct child of its server, so a *grand*child is something that
/// agent launched for itself — a sub-agent, a `claude` invoked by a tool — which
/// is precisely what the programmatic filter is there to keep out of the panel.
/// Walking the tree instead of looking one step up would let all of those back
/// in, one row each.
pub fn hosted_by(pid: u32, runtimes: &[Runtime]) -> Option<&Runtime> {
    let parent = crate::session::parent_pid(pid)?;
    runtimes.iter().find(|runtime| runtime.pid == parent)
}

/// A T3 Code thread, as much of one as the panel has any use for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    /// T3's own id for the thread — what a deep link points at.
    pub thread_id: String,
    /// The thread's title in T3's sidebar.
    pub title: String,
    /// The id the underlying agent knows the session by: for Claude Code the
    /// `--session-id` it was launched with, which is also what its hooks report.
    pub provider_session_id: Option<String>,
    /// Where the agent is working — the project directory or its worktree.
    pub cwd: Option<String>,
    /// What T3 is holding this thread for, if anything. Kept as the three asks
    /// T3 itself keeps apart rather than collapsed to a bool: its sidebar
    /// paints "Pending Approval", "Awaiting Input" and "Plan Ready" as three
    /// different states, the projection stores them as three counters, and a
    /// sum threw all of that away one column before vibewatch could use it.
    pub blocked: Option<Ask>,
}

/// Every thread T3 has a provider runtime for, newest first.
///
/// Read-only, and never anything else: this is another application's live
/// database, and vibewatch is a spectator to it. Failure is silent and total —
/// no T3 Code, an older schema, a database mid-migration — because the fallback
/// is the state vibewatch derives for itself from hooks and transcripts, which
/// is a complete picture already. The T3 read only ever adds the names and the
/// thread ids on top.
#[cfg(feature = "t3")]
pub fn threads(base_dir: &Path) -> Vec<Thread> {
    match read_threads(base_dir) {
        Ok(threads) => threads,
        Err(err) => {
            report_read_failure(base_dir, &err);
            Vec::new()
        }
    }
}

/// Without the `t3` feature the state database is not read at all: T3 sessions
/// still appear and still work, they just wear the agent's own title instead of
/// the thread's and cannot be pointed at by thread id.
#[cfg(not(feature = "t3"))]
pub fn threads(_base_dir: &Path) -> Vec<Thread> {
    Vec::new()
}

#[cfg(feature = "t3")]
fn read_threads(base_dir: &Path) -> rusqlite::Result<Vec<Thread>> {
    // `mode=ro` rather than opening the file read-only by flag alone: SQLite
    // needs the URI form to promise it will not write, and a writable open would
    // contend with the server for the database lock on every tick.
    let uri = format!(
        "file:{}?mode=ro",
        base_dir.join("state.sqlite").to_string_lossy()
    );
    let conn = rusqlite::Connection::open_with_flags(
        uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    // The server writes constantly; a checkpoint mid-read is a lock we wait out
    // rather than a tick we lose. Short, because this runs on the scan loop.
    conn.busy_timeout(std::time::Duration::from_millis(250))?;

    // The two projections are joined rather than read from `projection_threads`
    // alone because the id the agent knows itself by is only in the runtime row,
    // as the cursor T3 would resume the provider session from.
    let mut stmt = conn.prepare(
        "SELECT r.thread_id,
                t.title,
                json_extract(r.resume_cursor_json, '$.resume'),
                json_extract(r.runtime_payload_json, '$.cwd'),
                COALESCE(t.pending_approval_count, 0),
                COALESCE(t.pending_user_input_count, 0),
                COALESCE(t.has_actionable_proposed_plan, 0)
           FROM provider_session_runtime r
           JOIN projection_threads t USING (thread_id)
          WHERE t.deleted_at IS NULL
          ORDER BY r.last_seen_at DESC
          LIMIT 200",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Thread {
            thread_id: row.get(0)?,
            title: row.get(1)?,
            provider_session_id: row.get(2)?,
            cwd: row.get(3)?,
            blocked: ask_from_counts(row.get(4)?, row.get(5)?, row.get(6)?),
        })
    })?;
    rows.collect()
}

/// Which ask wins when a thread has more than one outstanding. Approval, then
/// input, then plan — T3's own order in `resolveThreadStatusPill`, and the right
/// one on its own terms: a permission gate has stopped the agent dead, a
/// question has stopped this turn, and a plan is only an invitation.
#[cfg(feature = "t3")]
fn ask_from_counts(approvals: i64, inputs: i64, plan: i64) -> Option<Ask> {
    if approvals > 0 {
        Some(Ask::Approval)
    } else if inputs > 0 {
        Some(Ask::Input)
    } else if plan > 0 {
        Some(Ask::Plan)
    } else {
        None
    }
}

/// Log a state database read that failed, once per distinct message.
///
/// Once, because this runs on the scan loop: a schema T3 has moved on from would
/// otherwise write the same line every three seconds for as long as the daemon
/// lives.
#[cfg(feature = "t3")]
fn report_read_failure(base_dir: &Path, err: &rusqlite::Error) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};

    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let message = err.to_string();
    let mut seen = SEEN.get_or_init(Default::default).lock().unwrap();
    if seen.insert(message.clone()) {
        eprintln!(
            "vibewatch: could not read T3 threads from {} ({message}) — T3 sessions keep the agent's own title",
            base_dir.display()
        );
    }
}

/// Find the thread a session belongs to.
///
/// The session id is the reliable match and the only one tried for Claude Code,
/// whose `--session-id` is exactly what T3 records to resume by. The directory
/// is the fallback for agents whose runtime cursor is shaped differently, and it
/// only answers when a single thread claims that directory — with two threads in
/// one worktree nothing here can tell which is which, and no name is better than
/// the wrong one.
pub fn match_thread<'a>(
    threads: &'a [Thread],
    session_id: &str,
    cwd: Option<&str>,
) -> Option<&'a Thread> {
    if let Some(thread) = threads
        .iter()
        .find(|thread| thread.provider_session_id.as_deref() == Some(session_id))
    {
        return Some(thread);
    }
    let cwd = cwd?;
    let mut matching = threads
        .iter()
        .filter(|thread| thread.cwd.as_deref() == Some(cwd));
    let first = matching.next()?;
    matching.next().is_none().then_some(first)
}

/// The id of the environment a profile's threads live in, which a deep link
/// needs alongside the thread id.
pub fn environment_id(base_dir: &Path) -> Option<String> {
    let id = std::fs::read_to_string(base_dir.join("environment-id")).ok()?;
    let id = id.trim();
    (!id.is_empty()).then(|| id.to_string())
}

/// Ask T3 Code to open a thread — the same move as selecting the agent's pane
/// inside its multiplexer before raising the window it lives in.
///
/// Off unless `t3.deep_link` is set, because as of T3 Code 0.0.33 nothing on the
/// other end listens: the app claims `t3code://` for its OAuth callbacks and a
/// second instance only reveals the window it already has. The URL is the shape
/// T3's own mobile widgets use, so it is the one a desktop handler would grow.
/// Until then the click still lands you in T3 Code — on whichever thread was
/// last open — and this stays quiet rather than handing xdg-open a URL that
/// would reopen the app or prompt for a handler.
///
/// Reads the config itself rather than being handed it: this is a click, once,
/// and it runs off a GTK signal that has no config to hand it.
pub fn focus_thread(thread_id: &str) {
    let Ok(config) = crate::config::Config::load() else {
        return;
    };
    if !config.t3.deep_link {
        return;
    }
    let Some(url) = live_runtimes()
        .iter()
        .find_map(|runtime| environment_id(&runtime.base_dir))
        .map(|environment| format!("t3code://threads/{environment}/{thread_id}"))
    else {
        return;
    };
    // Spawned and not waited on: the click's real job is the window raise that
    // follows, and `xdg-open` can take the better part of a second to hand the
    // URL over and exit.
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parenthood rule, which is what keeps a T3 thread's own sub-agents out
    /// of the panel: the agent is a child of the server, everything it launches
    /// is a grandchild.
    #[test]
    fn only_a_direct_child_of_the_server_is_hosted() {
        let me = std::process::id();
        let parent = crate::session::parent_pid(me).expect("own parent resolves");
        let runtimes = vec![Runtime {
            pid: parent,
            base_dir: PathBuf::from("/nowhere"),
        }];
        assert_eq!(hosted_by(me, &runtimes).map(|r| r.pid), Some(parent));

        // The grandparent hosts this process's parent, not this process.
        if let Some(grandparent) = crate::session::parent_pid(parent) {
            let runtimes = vec![Runtime {
                pid: grandparent,
                base_dir: PathBuf::from("/nowhere"),
            }];
            assert_eq!(hosted_by(me, &runtimes), None);
        }
    }

    #[test]
    fn nothing_is_hosted_without_a_running_server() {
        assert_eq!(hosted_by(std::process::id(), &[]), None);
    }

    #[test]
    fn a_stale_runtime_file_does_not_claim_a_recycled_pid() {
        // PID 1 is always alive and is never T3.
        assert!(!looks_like_t3_server(1));
        assert!(!looks_like_t3_server(u32::MAX));
    }

    /// The three counters are independent — a thread can have a plan on the
    /// table *and* a tool waiting on a yes — so the mapping has to rank them,
    /// and it ranks them T3's way: the gate that has stopped the agent dead
    /// comes before the question that stopped the turn, which comes before the
    /// plan that is only an invitation.
    #[cfg(feature = "t3")]
    #[test]
    fn the_ask_that_blocks_hardest_wins() {
        assert_eq!(ask_from_counts(0, 0, 0), None);
        assert_eq!(ask_from_counts(1, 0, 0), Some(Ask::Approval));
        assert_eq!(ask_from_counts(0, 2, 0), Some(Ask::Input));
        assert_eq!(ask_from_counts(0, 0, 1), Some(Ask::Plan));
        assert_eq!(ask_from_counts(1, 1, 1), Some(Ask::Approval));
        assert_eq!(ask_from_counts(0, 1, 1), Some(Ask::Input));
    }

    fn thread(id: &str, session: Option<&str>, cwd: Option<&str>) -> Thread {
        Thread {
            thread_id: id.into(),
            title: format!("thread {id}"),
            provider_session_id: session.map(Into::into),
            cwd: cwd.map(Into::into),
            blocked: None,
        }
    }

    #[test]
    fn the_session_id_matches_before_the_directory() {
        let threads = vec![
            thread("t1", Some("sess-a"), Some("/w/api")),
            thread("t2", Some("sess-b"), Some("/w/api")),
        ];
        let matched = match_thread(&threads, "sess-b", Some("/w/api"));
        assert_eq!(matched.map(|t| t.thread_id.as_str()), Some("t2"));
    }

    #[test]
    fn the_directory_answers_only_when_it_is_unambiguous() {
        let threads = vec![
            thread("t1", None, Some("/w/api")),
            thread("t2", None, Some("/w/web")),
        ];
        assert_eq!(
            match_thread(&threads, "unknown", Some("/w/web")).map(|t| t.thread_id.as_str()),
            Some("t2")
        );

        let shared = vec![
            thread("t1", None, Some("/w/api")),
            thread("t2", None, Some("/w/api")),
        ];
        assert_eq!(match_thread(&shared, "unknown", Some("/w/api")), None);
        assert_eq!(match_thread(&threads, "unknown", None), None);
    }
}
