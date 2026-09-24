//! One stable session key per agent session, shared by every process that
//! acts for it.
//!
//! An interactive session's hooks and its MCP server are different processes
//! with different pids, so `claims::owner_id()`'s pid-based fallback would
//! give each its own identity. Instead:
//!
//! - hooks key the session `<agent>:<session_id>` (every hook gets the host's
//!   session id on stdin) and register it with the pid of the agent CLI
//!   itself -- the first ancestor that isn't a shell wrapper;
//! - the MCP server (a child of that same agent CLI) finds the live session
//!   row whose pid is its own agent ancestor.
//!
//! A dispatched headless job already has one identity across processes,
//! `AGENTFLARE_CLAIM_OWNER` (or the in-process owner override), and it wins
//! everywhere. Its row is kept alive by the job's own heartbeat, so hooks
//! and the MCP server only refresh `last_seen_at` for it -- never its pid,
//! which belongs to the job, not to one short-lived agent turn.

use crate::sessions::{self, Touch};
use rusqlite::{Connection, OptionalExtension};

/// Process names skipped when walking up to the agent CLI: shells and
/// exec-wrappers a host may put between itself and a hook/MCP command.
const WRAPPERS: &[&str] = &[
    "sh",
    "bash",
    "dash",
    "zsh",
    "fish",
    "ksh",
    "env",
    "nice",
    "nohup",
    "timeout",
    "agentflare",
    "cmd.exe",
    "powershell",
    "pwsh",
];

/// Refresh `last_seen_at` at most this often from a hot path.
const TOUCH_EVERY_SECS: i64 = 30;

/// `(ppid, comm)` of `pid`.
#[cfg(target_os = "linux")]
fn parent_of(pid: u32) -> Option<(u32, String)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `pid (comm) state ppid ...` -- comm may itself contain spaces/parens.
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let comm = stat.get(open + 1..close)?.to_string();
    let ppid = stat
        .get(close + 1..)?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some((ppid, comm))
}

#[cfg(not(target_os = "linux"))]
fn parent_of(pid: u32) -> Option<(u32, String)> {
    let out = flare_process::command("ps")
        .args(["-o", "ppid=,comm=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.trim();
    let (ppid, comm) = line.split_once(char::is_whitespace)?;
    let comm = comm.trim();
    let comm = comm.rsplit('/').next().unwrap_or(comm).to_string();
    Some((ppid.trim().parse().ok()?, comm))
}

fn is_wrapper(comm: &str) -> bool {
    let base = comm.rsplit(['/', '\\']).next().unwrap_or(comm);
    let base = base.strip_suffix(".exe").unwrap_or(base);
    WRAPPERS.iter().any(|w| w.eq_ignore_ascii_case(base)) || base.starts_with("agentflare")
}

/// Pid of the agent CLI this process runs under: the first ancestor above
/// this process that isn't a shell/exec wrapper. Computed once.
pub fn agent_pid() -> Option<u32> {
    static PID: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *PID.get_or_init(|| agent_pid_from(std::process::id(), parent_of))
}

fn agent_pid_from(start: u32, parent_of: impl Fn(u32) -> Option<(u32, String)>) -> Option<u32> {
    let (mut pid, _) = parent_of(start)?;
    for _ in 0..8 {
        if pid <= 1 {
            return None;
        }
        let (ppid, comm) = parent_of(pid)?;
        if !is_wrapper(&comm) {
            return Some(pid);
        }
        pid = ppid;
    }
    None
}

/// The headless-job identity, when this process acts for one.
pub fn job_owner() -> Option<String> {
    let set = crate::claims::has_owner_override()
        || std::env::var("AGENTFLARE_CLAIM_OWNER").is_ok_and(|s| !s.is_empty());
    set.then(crate::claims::owner_id)
}

fn session_name() -> Option<String> {
    std::env::var("AGENTFLARE_SESSION_NAME")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The session key a hook acts for.
pub fn hook_key(agent: &str, session_id: &str) -> String {
    job_owner().unwrap_or_else(|| format!("{agent}:{session_id}"))
}

/// Registers/refreshes the hook's session: full registration (pid, name,
/// cwd) when `force` or the row is missing/ended, otherwise a throttled
/// `last_seen_at` bump. A job's row only ever gets `last_seen_at`.
pub fn touch_hook_session(
    conn: &Connection,
    key: &str,
    cwd: Option<&str>,
    force: bool,
    now: i64,
) -> rusqlite::Result<()> {
    let existing = sessions::get(conn, key)?;
    let fresh = existing
        .as_ref()
        .is_some_and(|s| s.ended_at.is_none() && now - s.last_seen_at < TOUCH_EVERY_SECS);
    if fresh && !force {
        return Ok(());
    }
    let is_job = job_owner().is_some();
    let name = if is_job { None } else { session_name() };
    sessions::touch(
        conn,
        &Touch {
            key,
            name: name.as_deref(),
            cwd: if is_job { None } else { cwd },
            pid: if is_job { None } else { agent_pid() },
            ..Default::default()
        },
        now,
    )
}

/// The newest live session registered with `pid` on this host.
fn live_session_by_pid(
    conn: &Connection,
    pid: u32,
    exclude: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT key FROM agent_sessions
         WHERE pid = ?1 AND host = ?2 AND ended_at IS NULL AND key != ?3
         ORDER BY last_seen_at DESC LIMIT 1",
        rusqlite::params![pid, sessions::this_host(), exclude],
        |r| r.get(0),
    )
    .optional()
}

/// Which session an MCP server / CLI process acts for, without registering
/// anything: the job owner, else the live session registered by this
/// process's agent CLI's hooks. `None` when neither applies.
pub fn attached_key(conn: &Connection) -> Option<String> {
    if let Some(owner) = job_owner() {
        return Some(owner);
    }
    let pid = agent_pid()?;
    live_session_by_pid(conn, pid, "").ok().flatten()
}

/// The key a long-lived MCP server acts for. Re-resolved per call (a host
/// `/clear` starts a new session id under the same pid). With no hook-
/// registered session to attach to (a host without hooks), registers this
/// process's own `claims::owner_id()` against the agent pid so it's still
/// listable and addressable; once a hook row for that pid appears, any mail
/// queued for the provisional key moves to it.
pub fn mcp_key(conn: &Connection, now: i64) -> String {
    if let Some(owner) = job_owner() {
        let _ = touch_hook_session(conn, &owner, None, false, now);
        return owner;
    }
    let provisional = crate::claims::owner_id();
    if let Some(pid) = agent_pid()
        && let Ok(Some(key)) = live_session_by_pid(conn, pid, &provisional)
    {
        if sessions::get(conn, &provisional)
            .ok()
            .flatten()
            .is_some_and(|s| s.ended_at.is_none())
        {
            let _ = crate::messages::reroute_undelivered(conn, &provisional, &key);
            let _ = sessions::end(conn, &provisional, now);
        }
        return key;
    }
    let cwd = std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    let _ = touch_hook_session(conn, &provisional, cwd.as_deref(), false, now);
    provisional
}

/// The sender key for a CLI invocation: the agent session it runs inside
/// (an agent shelling out to `agentflare message ...`), else the human at
/// the terminal, `human:<user>`.
pub fn cli_key(conn: &Connection) -> String {
    attached_key(conn).unwrap_or_else(human_key)
}

pub fn human_key() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "local".to_string());
    format!("human:{}", user.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_pid_walks_past_shell_wrappers() {
        // 10 (hook) <- 9 sh <- 8 bash <- 7 claude <- 1
        let tree = |pid: u32| -> Option<(u32, String)> {
            match pid {
                10 => Some((9, "agentflare".into())),
                9 => Some((8, "sh".into())),
                8 => Some((7, "bash".into())),
                7 => Some((1, "claude".into())),
                _ => None,
            }
        };
        assert_eq!(agent_pid_from(10, tree), Some(7));
        // MCP server spawned directly by the agent CLI.
        let direct = |pid: u32| match pid {
            20 => Some((7, "agentflare".to_string())),
            7 => Some((1, "node".to_string())),
            _ => None,
        };
        assert_eq!(agent_pid_from(20, direct), Some(7));
        // Only wrappers up to init: no agent.
        let orphan = |pid: u32| match pid {
            30 => Some((29, "agentflare".to_string())),
            29 => Some((1, "bash".to_string())),
            _ => None,
        };
        assert_eq!(agent_pid_from(30, orphan), None);
    }

    #[test]
    fn this_process_has_a_parent_on_linux() {
        #[cfg(target_os = "linux")]
        assert!(parent_of(std::process::id()).is_some());
    }

    #[test]
    fn mcp_key_attaches_to_the_hook_registered_session_by_pid() {
        if job_owner().is_some() {
            return; // running under a dispatched job: identity is fixed
        }
        let Some(pid) = agent_pid() else {
            return; // no non-wrapper ancestor in this environment
        };
        let c = Connection::open_in_memory().unwrap();
        sessions::migrate(&c).unwrap();
        crate::messages::migrate(&c).unwrap();
        // No hook row yet: provisional key, registered against the pid.
        let provisional = mcp_key(&c, 100);
        crate::messages::send(&c, "x:1", &provisional, "early", None, 100, |_| {
            Err(String::new())
        })
        .unwrap();
        // The hook registers the real session under the same agent pid.
        sessions::touch(
            &c,
            &Touch {
                key: "claude-code:sess-1",
                pid: Some(pid),
                ..Default::default()
            },
            101,
        )
        .unwrap();
        assert_eq!(mcp_key(&c, 102), "claude-code:sess-1");
        assert_eq!(attached_key(&c).as_deref(), Some("claude-code:sess-1"));
        let moved = crate::messages::take_undelivered(&c, "claude-code:sess-1", 5, 103).unwrap();
        assert_eq!(moved.len(), 1, "provisional mail follows the session");
        assert!(
            sessions::get(&c, &provisional)
                .unwrap()
                .unwrap()
                .ended_at
                .is_some()
        );
    }
}
