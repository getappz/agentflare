//! Live agent-session registry (`agent_sessions` in `agentflare.db`).
//!
//! One row per running agent session -- an interactive Claude Code/Codex
//! session, or a daemon-dispatched headless job -- keyed by the same owner
//! string its item claims carry (`claims::owner_id()`). Two things read it:
//! claim liveness (release a dead session's claims within a minute instead
//! of waiting out the multi-hour claim TTL) and inter-agent messaging
//! (addressing, listing who is live).
//!
//! Machine-local like every other `agentflare.db` table: sessions on another
//! host are only ever judged by `last_seen_at`, never by pid.

use rusqlite::{Connection, OptionalExtension, params};

pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_sessions (
            key TEXT PRIMARY KEY,
            agent TEXT NOT NULL,
            name TEXT,
            item_id TEXT,
            cwd TEXT,
            host TEXT NOT NULL,
            pid INTEGER,
            started_at INTEGER NOT NULL,
            last_seen_at INTEGER NOT NULL,
            ended_at INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_agent_sessions_live
            ON agent_sessions(ended_at, last_seen_at);",
    )
}

/// A session is presumed gone once it hasn't been seen this long AND its
/// liveness can't be checked by pid (another host, or no pid recorded).
/// Interactive sessions touch the registry on every hook/tool call; headless
/// jobs are refreshed by their run's heartbeat.
pub const STALE_AFTER_SECS: i64 = 15 * 60;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Session {
    pub key: String,
    pub agent: String,
    pub name: Option<String>,
    pub item_id: Option<String>,
    pub cwd: Option<String>,
    pub host: String,
    pub pid: Option<u32>,
    pub started_at: i64,
    pub last_seen_at: i64,
    pub ended_at: Option<i64>,
}

/// What a caller knows about a session when it registers/touches it. `None`
/// fields keep whatever the row already holds.
#[derive(Debug, Clone, Default)]
pub struct Touch<'a> {
    pub key: &'a str,
    pub name: Option<&'a str>,
    pub item_id: Option<&'a str>,
    pub cwd: Option<&'a str>,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    Live,
    Dead,
    /// No registry row: nothing to judge by (e.g. a session from before this
    /// registry existed). Callers fall back to their own TTL.
    Unknown,
}

pub fn this_host() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty())
        })
        .unwrap_or_else(|| "localhost".to_string())
}

/// Upserts `t.key` as live now. Clears `ended_at`: a key seen again is live.
pub fn touch(conn: &Connection, t: &Touch<'_>, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO agent_sessions
            (key, agent, name, item_id, cwd, host, pid, started_at, last_seen_at, ended_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, NULL)
         ON CONFLICT(key) DO UPDATE SET
            name = COALESCE(excluded.name, name),
            item_id = COALESCE(excluded.item_id, item_id),
            cwd = COALESCE(excluded.cwd, cwd),
            host = excluded.host,
            pid = COALESCE(excluded.pid, pid),
            last_seen_at = excluded.last_seen_at,
            ended_at = NULL",
        params![
            t.key,
            crate::claims::agent_of(t.key),
            t.name,
            t.item_id,
            t.cwd,
            this_host(),
            t.pid,
            now,
        ],
    )?;
    Ok(())
}

pub fn end(conn: &Connection, key: &str, now: i64) -> rusqlite::Result<bool> {
    Ok(conn.execute(
        "UPDATE agent_sessions SET ended_at = ?2 WHERE key = ?1 AND ended_at IS NULL",
        params![key, now],
    )? > 0)
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        key: r.get(0)?,
        agent: r.get(1)?,
        name: r.get(2)?,
        item_id: r.get(3)?,
        cwd: r.get(4)?,
        host: r.get(5)?,
        pid: r.get(6)?,
        started_at: r.get(7)?,
        last_seen_at: r.get(8)?,
        ended_at: r.get(9)?,
    })
}

const COLUMNS: &str =
    "key, agent, name, item_id, cwd, host, pid, started_at, last_seen_at, ended_at";

pub fn get(conn: &Connection, key: &str) -> rusqlite::Result<Option<Session>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM agent_sessions WHERE key = ?1"),
        [key],
        row,
    )
    .optional()
}

/// Resolves a user-facing address -- a session key, or a unique session
/// `name` -- to the live session it names.
pub fn resolve(conn: &Connection, address: &str, now: i64) -> rusqlite::Result<Option<Session>> {
    if let Some(s) = get(conn, address)? {
        return Ok(Some(s));
    }
    let named: Vec<Session> = list_live(conn, now)?
        .into_iter()
        .filter(|s| s.name.as_deref() == Some(address))
        .collect();
    Ok((named.len() == 1)
        .then(|| named.into_iter().next())
        .flatten())
}

/// Judges one session: an ended row is dead; a row on this host with a pid is
/// judged by whether that pid exists (exact, and works for a session idle
/// for hours); otherwise by `last_seen_at` against [`STALE_AFTER_SECS`].
pub fn liveness_of(s: &Session, now: i64) -> Liveness {
    if s.ended_at.is_some() {
        return Liveness::Dead;
    }
    if s.host == this_host()
        && let Some(pid) = s.pid
    {
        return if crate::ipc::process::is_alive(pid) {
            Liveness::Live
        } else {
            Liveness::Dead
        };
    }
    if now - s.last_seen_at <= STALE_AFTER_SECS {
        Liveness::Live
    } else {
        Liveness::Dead
    }
}

pub fn liveness(conn: &Connection, key: &str, now: i64) -> rusqlite::Result<Liveness> {
    Ok(get(conn, key)?.map_or(Liveness::Unknown, |s| liveness_of(&s, now)))
}

/// Every session not known to be dead, most recently seen first.
pub fn list_live(conn: &Connection, now: i64) -> rusqlite::Result<Vec<Session>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM agent_sessions WHERE ended_at IS NULL ORDER BY last_seen_at DESC"
    ))?;
    let rows = stmt
        .query_map([], row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter(|s| liveness_of(s, now) == Liveness::Live)
        .collect())
}

/// Drops rows ended or unseen for a week -- the registry is for live state.
pub fn prune(conn: &Connection, now: i64) -> rusqlite::Result<usize> {
    const KEEP_SECS: i64 = 7 * 24 * 3600;
    conn.execute(
        "DELETE FROM agent_sessions WHERE COALESCE(ended_at, last_seen_at) < ?1",
        [now - KEEP_SECS],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        c
    }

    fn touch_key(c: &Connection, key: &str, pid: Option<u32>, now: i64) {
        touch(
            c,
            &Touch {
                key,
                pid,
                ..Default::default()
            },
            now,
        )
        .unwrap();
    }

    #[test]
    fn a_local_session_is_judged_by_its_pid_not_its_age() {
        let c = conn();
        touch_key(&c, "claude-code:a", Some(std::process::id()), 0);
        assert_eq!(
            liveness(&c, "claude-code:a", 1_000_000).unwrap(),
            Liveness::Live
        );
        touch_key(&c, "claude-code:b", Some(u32::MAX - 7), 0);
        assert_eq!(liveness(&c, "claude-code:b", 1).unwrap(), Liveness::Dead);
    }

    #[test]
    fn a_session_without_pid_goes_stale_by_age_and_ended_is_dead() {
        let c = conn();
        touch_key(&c, "codex:x", None, 100);
        assert_eq!(
            liveness(&c, "codex:x", 100 + STALE_AFTER_SECS).unwrap(),
            Liveness::Live
        );
        assert_eq!(
            liveness(&c, "codex:x", 101 + STALE_AFTER_SECS).unwrap(),
            Liveness::Dead
        );
        touch_key(&c, "codex:y", None, 100);
        end(&c, "codex:y", 101).unwrap();
        assert_eq!(liveness(&c, "codex:y", 102).unwrap(), Liveness::Dead);
        assert_eq!(liveness(&c, "nobody:1", 102).unwrap(), Liveness::Unknown);
    }

    #[test]
    fn touch_keeps_known_fields_and_revives_an_ended_key() {
        let c = conn();
        touch(
            &c,
            &Touch {
                key: "claude-code:a",
                name: Some("planner"),
                item_id: Some("item-1"),
                ..Default::default()
            },
            1,
        )
        .unwrap();
        end(&c, "claude-code:a", 2).unwrap();
        touch_key(&c, "claude-code:a", None, 3);
        let s = get(&c, "claude-code:a").unwrap().unwrap();
        assert_eq!(s.name.as_deref(), Some("planner"));
        assert_eq!(s.item_id.as_deref(), Some("item-1"));
        assert_eq!((s.agent.as_str(), s.ended_at), ("claude-code", None));
        assert_eq!(
            resolve(&c, "planner", 3).unwrap().map(|s| s.key),
            Some("claude-code:a".into())
        );
    }
}
