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

use std::io::Write;
use std::os::unix::net::UnixStream;
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
    profile_dirs()
        .into_iter()
        .filter_map(|base_dir| {
            let pid = runtime_pid(&base_dir)?;
            looks_like_t3_server(pid).then_some(Runtime { pid, base_dir })
        })
        .collect()
}

/// Every T3 state directory that exists, most-likely first — one per build
/// channel, both of which can be in use at once.
pub(crate) fn profile_dirs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    PROFILES
        .iter()
        .map(|profile| home.join(".t3").join(profile))
        .filter(|dir| dir.is_dir())
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
    /// different states, and a bool threw all of that away one column before
    /// vibewatch could use it.
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

/// The file T3 keeps its state in.
///
/// `statev2.sqlite` since the orchestration-v2 migration, which left the old
/// `state.sqlite` in place as a frozen snapshot — so the name is not a detail
/// to be lenient about: reading the stale one succeeds, returns threads last
/// touched the day of the migration, and matches none of them.
#[cfg(feature = "t3")]
fn state_db_path(base_dir: &Path) -> PathBuf {
    base_dir.join("statev2.sqlite")
}

#[cfg(feature = "t3")]
fn read_threads(base_dir: &Path) -> rusqlite::Result<Vec<Thread>> {
    // `mode=ro` rather than opening the file read-only by flag alone: SQLite
    // needs the URI form to promise it will not write, and a writable open would
    // contend with the server for the database lock on every tick.
    let uri = format!("file:{}?mode=ro", state_db_path(base_dir).to_string_lossy());
    let conn = rusqlite::Connection::open_with_flags(
        uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    // The server writes constantly; a checkpoint mid-read is a lock we wait out
    // rather than a tick we lose. Short, because this runs on the scan loop.
    conn.busy_timeout(std::time::Duration::from_millis(250))?;

    // Four projections, because the thread row alone names none of what the
    // panel needs:
    //  - the id the agent knows itself by is the provider thread's
    //    `nativeThreadRef.nativeId` — the `--session-id` Claude was launched
    //    with, and the `threadId` for Codex. Not the `provider_session_id`
    //    column, which is T3's own composite id and whose trailing uuid is an
    //    *earlier* session of the same thread.
    //  - the provider thread is pinned to the thread's own
    //    `active_provider_thread_id` so a thread that has handed off or forked
    //    contributes one row, the current one, rather than one per generation.
    //  - a thread outside a worktree has no `worktreePath` and works in its
    //    project's root, which is the directory its agent reports.
    //  - the asks are rows now, not counters: one pending runtime request at a
    //    time, split by kind exactly as T3 splits it for its own sidebar, and a
    //    proposed plan is `active` until something supersedes it.
    let mut stmt = conn.prepare(
        "SELECT t.thread_id,
                t.title,
                json_extract(p.payload_json, '$.nativeThreadRef.nativeId'),
                COALESCE(
                    json_extract(t.payload_json, '$.worktreePath'),
                    j.workspace_root
                ),
                EXISTS (SELECT 1
                          FROM orchestration_v2_projection_runtime_requests q
                         WHERE q.thread_id = t.thread_id
                           AND q.status = 'pending'
                           AND q.kind NOT IN ('user_input', 'auth_refresh')),
                EXISTS (SELECT 1
                          FROM orchestration_v2_projection_runtime_requests q
                         WHERE q.thread_id = t.thread_id
                           AND q.status = 'pending'
                           AND q.kind = 'user_input'),
                EXISTS (SELECT 1
                          FROM orchestration_v2_projection_plans n
                         WHERE n.thread_id = t.thread_id
                           AND n.kind = 'proposed_plan'
                           AND n.status = 'active'),
                t.interaction_mode
           FROM orchestration_v2_projection_provider_threads p
           JOIN orchestration_v2_projection_threads t
             ON p.provider_thread_id = t.active_provider_thread_id
           LEFT JOIN projection_projects j ON j.project_id = t.project_id
          WHERE t.deleted_at IS NULL
          ORDER BY p.updated_at DESC
          LIMIT 200",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Thread {
            thread_id: row.get(0)?,
            title: row.get(1)?,
            provider_session_id: row.get(2)?,
            cwd: row.get(3)?,
            blocked: ask_from_counts(
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get_ref(7)?.as_str()?,
            ),
        })
    })?;
    rows.collect()
}

/// Which ask wins when a thread has more than one outstanding. Approval, then
/// input, then plan — T3's own order in `resolveThreadStatusPill`, and the right
/// one on its own terms: a permission gate has stopped the agent dead, a
/// question has stopped this turn, and a plan is only an invitation.
///
/// Three of T3's four ranks. The fourth — a running turn outranks a plan too —
/// needs to know what the session is doing, which this side cannot see, so it
/// lives in [`SessionRegistry::apply_t3_thread`](crate::session::SessionRegistry::apply_t3_thread);
/// a change to T3's pill has to be answered in both places.
///
/// The plan flag is the one that needs a second opinion, because it marks
/// nothing outstanding: a `proposed_plan` stays `active` until something
/// supersedes it, and approving one and watching the agent build it never
/// does. What ends the ask is the thread leaving plan mode, which is what
/// accepting a plan does — so the mode is the condition T3's own pill checks
/// alongside the flag, and without it a thread says `plan ready` for the rest
/// of its life, from the moment it was told to go.
///
/// The three arguments arrive as `EXISTS` results, 0 or 1 — the projection
/// keeps one pending runtime request per thread, so there is nothing to count.
#[cfg(feature = "t3")]
fn ask_from_counts(approvals: i64, inputs: i64, plan: i64, interaction_mode: &str) -> Option<Ask> {
    if approvals > 0 {
        Some(Ask::Approval)
    } else if inputs > 0 {
        Some(Ask::Input)
    } else if plan > 0 && interaction_mode == "plan" {
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
/// The URL is the shape T3's own mobile widgets use, which is what a desktop
/// handler grows into. On by default because every way it can miss is
/// harmless: a T3 that does not route the link still reveals its window — the
/// click's other half — so the worst case is the behaviour we had before, plus
/// a spawned `xdg-open`. Two cases are worth knowing about, and `t3.deep_link`
/// exists for the second: with T3 closed the URL launches it, and on a machine
/// with no T3 desktop app the scheme has no handler at all and the desktop asks
/// which application to open it with.
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
    // Hand it to the app directly when it is listening. `xdg-open` routes the
    // same URL, but by launching a whole second copy of T3 Code whose only job
    // is to take the single-instance lock, forward the link and die — measured
    // at ~1.4s, half of it an AppImage's squashfs mount. Writing to the socket
    // is a connect and a write.
    if write_deep_link_socket(&url).is_ok() {
        return;
    }
    // Spawned and not waited on: the click's real job is the window raise that
    // follows, and `xdg-open` can take the better part of a second to hand the
    // URL over and exit.
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// Path T3 Code listens on for links, mirroring its own `deepLinkSocketPath`.
///
/// Only the release channel is addressed: a dev build binds its own name, and a
/// click in the panel is about the agent the user is actually running.
///
/// `None` without `XDG_RUNTIME_DIR`. T3 Code also answers on a uid-scoped name
/// in the temp dir, but reproducing that would cost a dependency for one
/// `getuid()` — and the variable is set on every desktop this panel runs on.
/// Falling back to `xdg-open` there is slower, not broken.
fn deep_link_socket_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    (!dir.is_empty()).then(|| PathBuf::from(dir).join("t3code-deeplink.sock"))
}

/// Write one URL and close. An error means nothing is listening — an older T3,
/// a dev-only build, or the app not running — and the caller falls back.
fn write_deep_link_socket(url: &str) -> std::io::Result<()> {
    let path =
        deep_link_socket_path().ok_or_else(|| std::io::Error::other("no XDG_RUNTIME_DIR"))?;
    let mut stream = UnixStream::connect(path)?;
    // The reader takes the first line, so the newline is what ends the message
    // rather than the close: a caller that lingers must not hold it open.
    stream.write_all(url.as_bytes())?;
    stream.write_all(b"\n")
}

/// Where T3 Code's MCP server listens, and the token that gets in.
///
/// Both are read from the hosted agent's own command line: T3 launches it with
/// `--mcp-config {"mcpServers":{"t3-code":{"url":…,"headers":{"Authorization":
/// "Bearer …"}}}}`, which `/proc/<pid>/cmdline` keeps readable for as long as
/// the agent runs. There is no token to mint — the server hands them out per
/// agent session — and borrowing the one T3 already trusts for this very agent
/// is the right scope rather than a shortcut: it can act on that thread and on
/// nothing else, and it dies with the session it belongs to.
///
/// This is the one thing in this module that is not a read. The state database
/// is still spectated and never written; acting on a thread goes through the
/// same tools T3 hands its own agents, so T3 stays the only writer of its own
/// state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub url: String,
    pub token: String,
}

/// The MCP endpoint a T3-hosted agent was given, if it still has one.
pub fn endpoint(agent_pid: u32) -> Option<Endpoint> {
    let raw = std::fs::read(format!("/proc/{agent_pid}/cmdline")).ok()?;
    let args: Vec<&[u8]> = raw.split(|b| *b == 0).collect();
    let config = args
        .iter()
        .position(|arg| *arg == b"--mcp-config")
        .and_then(|i| args.get(i + 1))
        .and_then(|arg| std::str::from_utf8(arg).ok())?;
    parse_endpoint(config)
}

/// The `--mcp-config` payload, reduced to the one server we care about.
///
/// Split out from [`endpoint`] so the shape T3 passes can be tested without a
/// process to read it from.
fn parse_endpoint(config: &str) -> Option<Endpoint> {
    let value: serde_json::Value = serde_json::from_str(config).ok()?;
    let server = value.get("mcpServers")?.get("t3-code")?;
    let url = server.get("url")?.as_str()?.to_string();
    let token = server
        .get("headers")?
        .get("Authorization")?
        .as_str()?
        .strip_prefix("Bearer ")?
        .to_string();
    Some(Endpoint { url, token })
}

/// The MCP revision this speaks. Sent on every call after the handshake, which
/// is how the server knows it does not have to guess at an older client.
#[cfg(feature = "t3")]
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// How long any one MCP round trip may take. Generous for a loopback call,
/// which comes back in single-digit milliseconds, because the cost of being
/// wrong is a wedged click handler rather than a slow one.
#[cfg(feature = "t3")]
const MCP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Answer T3's own pending question for `thread_id` with the option the user
/// clicked in the panel.
///
/// The whole point of going through T3 rather than through the agent's
/// permission hook: T3 hosts the prompt for the threads it runs, so a hook that
/// answers behind its back leaves the question outstanding in T3 forever — its
/// card stays up, its sidebar stays on `Awaiting Input`, and vibewatch's own T3
/// poller keeps flipping the session back to blocked while the agent works. One
/// owner for one question, and it is the one with the UI.
///
/// Five round trips on loopback: the handshake, the list, the read, the answer.
/// The list and the read are what turn "the user clicked the second button"
/// into the ids and the option value T3's own composer would have sent.
#[cfg(feature = "t3")]
pub fn answer_question(endpoint: &Endpoint, thread_id: &str, label: &str) -> anyhow::Result<()> {
    use anyhow::Context;

    let mut mcp = Mcp::connect(endpoint)?;
    let listed = mcp.call(
        "t3_pending_request_list",
        serde_json::json!({ "threadId": thread_id }),
    )?;
    let request_id = listed
        .get("requestIds")
        .and_then(|ids| ids.get(0))
        .and_then(|id| id.as_str())
        .context("T3 has no pending question for this thread")?
        .to_string();
    let read = mcp.call(
        "t3_pending_request_read",
        serde_json::json!({ "threadId": thread_id, "requestId": request_id }),
    )?;
    let (question_id, value) =
        answer_for(&read, label).context("the clicked option is not one T3 is offering")?;
    mcp.call(
        "t3_pending_request_respond",
        serde_json::json!({
            "threadId": thread_id,
            "requestId": request_id,
            "answers": { question_id: value },
        }),
    )?;
    Ok(())
}

/// Which question the clicked label belongs to, and the value T3 wants back for
/// it — `option.value` when the provider gave one, the label otherwise, which
/// is the same fallback T3's own composer applies.
///
/// Searches every question rather than assuming the first: the panel only grows
/// buttons for the single-question shape today, and this way it is the label
/// that decides rather than an index that would silently answer the wrong one.
#[cfg(feature = "t3")]
fn answer_for(read: &serde_json::Value, label: &str) -> Option<(String, String)> {
    for question in read.get("questions")?.as_array()? {
        let id = question.get("id")?.as_str()?;
        for option in question.get("options")?.as_array()? {
            if option.get("label").and_then(|l| l.as_str()) != Some(label) {
                continue;
            }
            let value = option
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or(label)
                .to_string();
            return Some((id.to_string(), value));
        }
    }
    None
}

/// One MCP session over T3's HTTP transport, open just long enough to answer.
///
/// Plain JSON both ways — the server answers `application/json` for every call
/// made here, so none of the streaming half of the protocol is needed — but the
/// handshake is not optional: a bare `tools/call` is a 400 until `initialize`
/// has issued the session id that every later request carries.
#[cfg(feature = "t3")]
struct Mcp {
    agent: ureq::Agent,
    url: String,
    token: String,
    session_id: String,
}

#[cfg(feature = "t3")]
impl Mcp {
    fn connect(endpoint: &Endpoint) -> anyhow::Result<Self> {
        use anyhow::Context;

        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(MCP_TIMEOUT))
            .build()
            .into();
        let response = agent
            .post(&endpoint.url)
            .header("Authorization", &format!("Bearer {}", endpoint.token))
            .header("Accept", "application/json, text/event-stream")
            .send_json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "vibewatch", "version": env!("CARGO_PKG_VERSION") },
                },
            }))
            .context("T3 Code refused the MCP handshake")?;
        let session_id = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .context("T3 Code issued no MCP session id")?
            .to_string();
        let mcp = Self {
            agent,
            url: endpoint.url.clone(),
            token: endpoint.token.clone(),
            session_id,
        };
        // Fire-and-forget by protocol: a notification has no id and no reply.
        mcp.post(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        }))?;
        Ok(mcp)
    }

    /// Invoke one tool and return what it answered, as T3's own schema for it.
    ///
    /// `structuredContent` is the same object the tool's success schema
    /// describes; the `content` text beside it is that object serialised for a
    /// model to read, and is the fallback for a tool that sends only the text.
    fn call(&mut self, tool: &str, arguments: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        use anyhow::{bail, Context};

        let body = self.post(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments },
        }))?;
        if let Some(error) = body.get("error") {
            bail!("T3 Code rejected {tool}: {error}");
        }
        let result = body.get("result").context("T3 Code sent no result")?;
        if result.get("isError").and_then(|flag| flag.as_bool()) == Some(true) {
            bail!("{tool} failed: {}", tool_text(result).unwrap_or_default());
        }
        if let Some(structured) = result.get("structuredContent") {
            return Ok(structured.clone());
        }
        let text = tool_text(result).context("T3 Code sent an empty result")?;
        serde_json::from_str(&text).context("T3 Code sent a result that is not JSON")
    }

    fn post(&self, body: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        use anyhow::Context;

        let response = self
            .agent
            .post(&self.url)
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Accept", "application/json, text/event-stream")
            .header("mcp-session-id", &self.session_id)
            .header("mcp-protocol-version", MCP_PROTOCOL_VERSION)
            .send_json(body)
            .context("T3 Code refused the MCP call")?;
        // A notification answers `202 Accepted` with no body at all.
        Ok(response.into_body().read_json().unwrap_or(serde_json::Value::Null))
    }
}

/// The text half of a tool result, which carries the error message when a tool
/// fails and the payload when it has no structured schema.
#[cfg(feature = "t3")]
fn tool_text(result: &serde_json::Value) -> Option<String> {
    Some(
        result
            .get("content")?
            .as_array()?
            .iter()
            .filter_map(|block| block.get("text")?.as_str())
            .collect::<Vec<_>>()
            .join(""),
    )
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
        // In plan mode throughout: the mode is the plan branch's own gate, and
        // holding it still here leaves the counters as the only variable.
        assert_eq!(ask_from_counts(0, 0, 0, "plan"), None);
        assert_eq!(ask_from_counts(1, 0, 0, "plan"), Some(Ask::Approval));
        assert_eq!(ask_from_counts(0, 2, 0, "plan"), Some(Ask::Input));
        assert_eq!(ask_from_counts(0, 0, 1, "plan"), Some(Ask::Plan));
        assert_eq!(ask_from_counts(1, 1, 1, "plan"), Some(Ask::Approval));
        assert_eq!(ask_from_counts(0, 1, 1, "plan"), Some(Ask::Input));
    }

    /// The go is spelled by the mode, not by the flag: accepting a plan puts
    /// the thread back in `default` and leaves `has_actionable_proposed_plan`
    /// standing, so a thread that was told to go and did the work must not
    /// still be asking for a verdict.
    #[cfg(feature = "t3")]
    #[test]
    fn an_accepted_plan_stops_asking() {
        assert_eq!(ask_from_counts(0, 0, 1, "default"), None);
        // And the mode gates that branch alone: the other two are live counts
        // and mean what they say whatever mode the thread is in.
        assert_eq!(ask_from_counts(1, 0, 1, "default"), Some(Ask::Approval));
        assert_eq!(ask_from_counts(0, 1, 1, "default"), Some(Ask::Input));
    }

    /// A T3 state database with just the columns [`read_threads`] reads.
    #[cfg(feature = "t3")]
    fn write_state_db(dir: &Path) {
        let conn = rusqlite::Connection::open(state_db_path(dir)).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE orchestration_v2_projection_provider_threads (
                provider_thread_id TEXT PRIMARY KEY,
                thread_id TEXT,
                payload_json TEXT,
                updated_at TEXT
            );
            CREATE TABLE orchestration_v2_projection_threads (
                thread_id TEXT PRIMARY KEY,
                project_id TEXT,
                title TEXT,
                interaction_mode TEXT,
                active_provider_thread_id TEXT,
                payload_json TEXT,
                deleted_at TEXT
            );
            CREATE TABLE orchestration_v2_projection_runtime_requests (
                thread_id TEXT,
                kind TEXT,
                status TEXT
            );
            CREATE TABLE orchestration_v2_projection_plans (
                thread_id TEXT,
                kind TEXT,
                status TEXT
            );
            CREATE TABLE projection_projects (
                project_id TEXT PRIMARY KEY,
                workspace_root TEXT
            );
            INSERT INTO projection_projects VALUES ('p-api', '/w/api');
            INSERT INTO orchestration_v2_projection_provider_threads VALUES
                ('pt-worktree', 't-worktree',
                 '{"nativeThreadRef":{"nativeId":"claude-session"}}',
                 '2026-10-05T00:00:01Z'),
                ('pt-root', 't-root',
                 '{"nativeThreadRef":{"nativeId":"codex-session"}}',
                 '2026-10-05T00:00:00Z'),
                ('pt-superseded', 't-worktree',
                 '{"nativeThreadRef":{"nativeId":"handed-off-session"}}',
                 '2026-10-05T00:00:02Z');
            INSERT INTO orchestration_v2_projection_threads VALUES
                ('t-worktree', 'p-api', 'Worktree thread', 'default', 'pt-worktree',
                 '{"worktreePath":"/w/api-feature"}', NULL),
                ('t-root', 'p-api', 'Project-root thread', 'default', 'pt-root',
                 '{"worktreePath":null}', NULL);
            "#,
        )
        .unwrap();
    }

    /// The session id is the provider thread's *native* id — the `--session-id`
    /// the agent was launched with — and not T3's own composite
    /// `provider_session_id`, whose trailing uuid is an earlier session of the
    /// same thread and so matches nothing the hooks report.
    #[cfg(feature = "t3")]
    #[test]
    fn reads_the_native_session_id_of_the_active_provider_thread() {
        let dir = tempfile::tempdir().unwrap();
        write_state_db(dir.path());

        let threads = read_threads(dir.path()).unwrap();
        // `pt-superseded` is the newest row of all and still contributes
        // nothing: a thread that handed off has one current provider thread,
        // and a second row would be a second card for one conversation.
        assert_eq!(threads.len(), 2);
        assert_eq!(
            threads[0].provider_session_id.as_deref(),
            Some("claude-session")
        );
        assert_eq!(
            threads[1].provider_session_id.as_deref(),
            Some("codex-session")
        );
    }

    /// Where the agent is working: its worktree, or — for a thread that has
    /// none — the root of the project it belongs to.
    #[cfg(feature = "t3")]
    #[test]
    fn a_thread_outside_a_worktree_reports_its_project_root() {
        let dir = tempfile::tempdir().unwrap();
        write_state_db(dir.path());

        let threads = read_threads(dir.path()).unwrap();
        assert_eq!(threads[0].cwd.as_deref(), Some("/w/api-feature"));
        assert_eq!(threads[1].cwd.as_deref(), Some("/w/api"));
    }

    /// The asks are rows now. One pending runtime request per thread, split by
    /// kind the way T3 splits it for its own sidebar: `user_input` is the
    /// question, `auth_refresh` is the app's own business and no ask at all,
    /// and everything else is a gate the agent is stopped at.
    #[cfg(feature = "t3")]
    #[test]
    fn a_pending_runtime_request_becomes_the_ask_its_kind_means() {
        let dir = tempfile::tempdir().unwrap();
        write_state_db(dir.path());
        let conn = rusqlite::Connection::open(state_db_path(dir.path())).unwrap();
        // The only pending request, replaced each round: the ask it produces.
        let ask_for = |kind: &str| {
            conn.execute(
                "DELETE FROM orchestration_v2_projection_runtime_requests",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO orchestration_v2_projection_runtime_requests
                 VALUES ('t-worktree', ?1, 'pending')",
                [kind],
            )
            .unwrap();
            read_threads(dir.path()).unwrap()[0].blocked
        };

        assert_eq!(read_threads(dir.path()).unwrap()[0].blocked, None);
        assert_eq!(ask_for("permission"), Some(Ask::Approval));
        assert_eq!(ask_for("file-change"), Some(Ask::Approval));
        assert_eq!(ask_for("user_input"), Some(Ask::Input));
        // Refreshing its own credentials is not a question for the user.
        assert_eq!(ask_for("auth_refresh"), None);

        // A resolved request is not an ask either.
        ask_for("permission");
        conn.execute(
            "UPDATE orchestration_v2_projection_runtime_requests SET status = 'resolved'",
            [],
        )
        .unwrap();
        assert_eq!(read_threads(dir.path()).unwrap()[0].blocked, None);
    }

    /// A proposed plan asks for a verdict only while the thread is still in
    /// plan mode — and `active` is how the projection spells "not superseded",
    /// which outlives the go-ahead.
    #[cfg(feature = "t3")]
    #[test]
    fn an_active_proposed_plan_asks_only_in_plan_mode() {
        let dir = tempfile::tempdir().unwrap();
        write_state_db(dir.path());
        let conn = rusqlite::Connection::open(state_db_path(dir.path())).unwrap();
        conn.execute(
            "INSERT INTO orchestration_v2_projection_plans
             VALUES ('t-worktree', 'proposed_plan', 'active')",
            [],
        )
        .unwrap();

        assert_eq!(read_threads(dir.path()).unwrap()[0].blocked, None);
        conn.execute(
            "UPDATE orchestration_v2_projection_threads
                SET interaction_mode = 'plan' WHERE thread_id = 't-worktree'",
            [],
        )
        .unwrap();
        assert_eq!(
            read_threads(dir.path()).unwrap()[0].blocked,
            Some(Ask::Plan)
        );
    }

    /// A deleted thread is gone from the panel, whatever its provider thread
    /// still says.
    #[cfg(feature = "t3")]
    #[test]
    fn a_deleted_thread_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        write_state_db(dir.path());
        let conn = rusqlite::Connection::open(state_db_path(dir.path())).unwrap();
        conn.execute(
            "UPDATE orchestration_v2_projection_threads
                SET deleted_at = '2026-10-05T00:00:00Z' WHERE thread_id = 't-worktree'",
            [],
        )
        .unwrap();

        let threads = read_threads(dir.path()).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].thread_id, "t-root");
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

    /// The exact `--mcp-config` shape T3 launches a Claude thread with, which
    /// is where the endpoint and its token come from.
    #[test]
    fn the_mcp_endpoint_comes_off_the_agents_own_command_line() {
        let config = r#"{"mcpServers":{"t3-code":{"type":"http","url":"http://127.0.0.1:3773/mcp","headers":{"Authorization":"Bearer tok-123"},"timeout":3900000}}}"#;
        assert_eq!(
            parse_endpoint(config),
            Some(Endpoint {
                url: "http://127.0.0.1:3773/mcp".to_string(),
                token: "tok-123".to_string(),
            })
        );

        // Another app's MCP config, or a token shape we do not understand:
        // nothing to answer on, and the caller falls back to T3's own UI.
        assert_eq!(parse_endpoint(r#"{"mcpServers":{"other":{"url":"x"}}}"#), None);
        assert_eq!(
            parse_endpoint(r#"{"mcpServers":{"t3-code":{"url":"x","headers":{"Authorization":"tok"}}}}"#),
            None,
            "a header that is not a Bearer is not a token"
        );
        assert_eq!(parse_endpoint("not json"), None);
    }

    /// What the click has to become: T3 wants the option's `value` when the
    /// provider gave one, and the label otherwise — the same fallback its own
    /// composer applies, so an answer from the panel is indistinguishable from
    /// one typed in the app.
    #[cfg(feature = "t3")]
    #[test]
    fn the_clicked_label_becomes_the_value_t3_asked_for() {
        let read = serde_json::json!({
            "requestId": "r-1",
            "questions": [
                { "id": "q1", "options": [{ "label": "Medium" }, { "label": "High" }] },
                { "id": "q2", "options": [{ "label": "Later", "value": "defer" }] },
            ],
        });
        assert_eq!(
            answer_for(&read, "High"),
            Some(("q1".to_string(), "High".to_string()))
        );
        assert_eq!(
            answer_for(&read, "Later"),
            Some(("q2".to_string(), "defer".to_string())),
            "the provider's value wins over the label it is drawn as"
        );
        // A label T3 is not offering: the row is stale, and answering anyway
        // would put a made-up string in the thread.
        assert_eq!(answer_for(&read, "Urgent"), None);
    }
}
