//! Work-claim ledger — a race-safe, leased lock so multiple AI agents don't
//! both grab the same GitHub issue/PR. A claim = "owner holds target in repo".
//! Backed by SQLite (`agentflare.db`); the whole point is that acquire and
//! stale-steal are ONE atomic statement, which a filesystem lock can't give
//! for the steal case. Models [Beads](https://github.com/gastownhall/beads)'s
//! claim/close model, minus the full issue-tracker surface.
//!
//! `acquire`/`heartbeat`/`release`/`done` are thin wrappers over
//! `db_kit::claim::ClaimLedger` — the generic, table/key-agnostic version of
//! this same atomic-upsert lease pattern, shared with
//! `agentflare-backend`'s `item_claims`. `migrate`/`list` stay as direct SQL
//! here rather than delegating: this table's `git_commit` column is a
//! git-provenance concern specific to the GitHub-issue/PR claim use case, not
//! something the generic ledger models (bolting it onto the generic API for
//! one caller would defeat the point of generalizing) — see `acquire()`
//! below for how it's threaded through instead.
pub use db_kit::claim::Acquire;
use db_kit::claim::ClaimLedger;
use rusqlite::{Connection, params};

/// Default lease: a claim whose owner hasn't heartbeat within this window is
/// stealable, so a crashed/hung agent can't wedge a target forever.
const DEFAULT_TTL_SECS: u64 = 1800; // 30 min

const LEDGER: ClaimLedger = ClaimLedger::new("claims", &["repo", "target"]);

pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS claims (
            repo         TEXT NOT NULL,
            target       TEXT NOT NULL,
            owner        TEXT NOT NULL,
            status       TEXT NOT NULL,
            created_at   INTEGER NOT NULL,
            heartbeat_at INTEGER NOT NULL,
            git_commit   TEXT,
            PRIMARY KEY (repo, target)
        );",
    )
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Claim {
    pub repo: String,
    pub target: String,
    pub owner: String,
    pub status: String,
    pub created_at: i64,
    pub heartbeat_at: i64,
    pub git_commit: Option<String>,
    /// Heartbeat older than the TTL — the claim is effectively available.
    pub stale: bool,
}

/// Attempts to claim `target`. Delegates the atomic acquire/steal upsert to
/// the generic ledger, then — only once we actually hold the claim — stores
/// `git_commit` with a follow-up UPDATE. `git_commit` isn't a column the
/// generic `ClaimLedger` knows about, so it can't be part of the atomic
/// upsert itself; this is a deliberate two-step, not an oversight, and it's
/// safe because the second statement only ever touches a row we just won.
pub fn acquire(
    conn: &Connection,
    repo: &str,
    target: &str,
    owner: &str,
    git_commit: Option<&str>,
    now: i64,
    ttl_secs: i64,
) -> rusqlite::Result<Acquire> {
    let outcome = LEDGER.acquire(conn, &[repo, target], owner, now, ttl_secs)?;
    if outcome == Acquire::Acquired {
        // Scoped to owner: if another owner steals the lease between LEDGER.acquire()
        // and this UPDATE, this must not overwrite their row's provenance with ours.
        conn.execute(
            "UPDATE claims SET git_commit = ?3 WHERE repo = ?1 AND target = ?2 AND owner = ?4",
            params![repo, target, git_commit, owner],
        )?;
    }
    Ok(outcome)
}

/// Refreshes the lease on a claim we own. Returns false if the claim is gone
/// or owned by someone else (don't heartbeat what isn't yours).
pub fn heartbeat(
    conn: &Connection,
    repo: &str,
    target: &str,
    owner: &str,
    now: i64,
) -> rusqlite::Result<bool> {
    LEDGER.heartbeat(conn, &[repo, target], owner, now)
}

/// Drops our claim entirely (frees the target). Owner-scoped.
pub fn release(conn: &Connection, repo: &str, target: &str, owner: &str) -> rusqlite::Result<bool> {
    LEDGER.release(conn, &[repo, target], owner)
}

/// Marks our claim done (kept for audit; a done target is re-acquirable).
pub fn done(
    conn: &Connection,
    repo: &str,
    target: &str,
    owner: &str,
    now: i64,
) -> rusqlite::Result<bool> {
    LEDGER.done(conn, &[repo, target], owner, now)
}

/// Lists claims (optionally scoped to `repo`). With `include_stale = false`,
/// only live `claimed` rows are returned — stale or done claims count as
/// available and are hidden.
pub fn list(
    conn: &Connection,
    repo: Option<&str>,
    include_stale: bool,
    now: i64,
    ttl_secs: i64,
) -> rusqlite::Result<Vec<Claim>> {
    let stale_before = now - ttl_secs;
    let mut stmt = conn.prepare(
        "SELECT repo, target, owner, status, created_at, heartbeat_at, git_commit
         FROM claims
         WHERE (?1 IS NULL OR repo = ?1)
         ORDER BY repo, target",
    )?;
    let rows = stmt.query_map(params![repo], |r| {
        let heartbeat_at: i64 = r.get(5)?;
        let status: String = r.get(3)?;
        Ok(Claim {
            repo: r.get(0)?,
            target: r.get(1)?,
            owner: r.get(2)?,
            stale: status == "claimed" && heartbeat_at < stale_before,
            status,
            created_at: r.get(4)?,
            heartbeat_at,
            git_commit: r.get(6)?,
        })
    })?;
    let all: Vec<Claim> = rows.collect::<Result<_, _>>()?;
    Ok(if include_stale {
        all
    } else {
        all.into_iter()
            .filter(|c| c.status == "claimed" && !c.stale)
            .collect()
    })
}

// --- identity / config resolution (impure; thin wrappers over env + git) ---

/// `<agent>:<instance>` — same agent chain as handoff, plus an instance
/// discriminator so two parallel sessions of one agent are distinct owners.
///
/// Instance is `AGENTFLARE_SESSION` if set, else the process pid. A long-lived
/// MCP server has a stable pid, so all its `claim_*` calls share one owner —
/// the common case. The CLI, however, is a fresh process per command, so
/// `AGENTFLARE_SESSION` must be set to keep ownership continuous across
/// separate `agentflare claim` invocations (acquire in one, release in
/// another); otherwise each command is a distinct owner.
pub fn owner_id() -> String {
    let agent = std::env::var("AGENTFLARE_AGENT")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(agent_detector::agent_name)
        .unwrap_or_else(|| "cli".to_string());
    let instance = std::env::var("AGENTFLARE_SESSION")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| std::process::id().to_string());
    format!("{agent}:{instance}")
}

/// Strips the `:<instance>` suffix off an owner id, leaving the stable agent
/// identity. Unlike claim ownership (deliberately instance-scoped, see
/// `owner_id` above), authorship of a comment should survive across
/// sessions — an agent restarting shouldn't lose the ability to edit its own
/// words just because its instance suffix changed.
pub fn agent_of(owner_id: &str) -> &str {
    owner_id.split(':').next().unwrap_or(owner_id)
}

pub fn ttl_secs() -> i64 {
    std::env::var("AGENTFLARE_CLAIM_TTL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TTL_SECS) as i64
}

pub fn now() -> i64 {
    db_kit::ids::now()
}

/// Normalizes a git remote URL to a stable `owner/name` key, so https and ssh
/// forms of the same repo map to one claim namespace.
/// `https://github.com/getappz/agentflare.git` and
/// `git@github-alias:getappz/agentflare.git` both → `getappz/agentflare`.
pub use crate::github::identity::normalize_repo;

/// Resolves the repo key: explicit `--repo` wins, else normalize the origin
/// remote from git provenance.
pub fn resolve_repo(explicit: Option<String>) -> Option<String> {
    explicit.filter(|s| !s.is_empty()).or_else(|| {
        crate::mcp_server::AgentflareMcp::git_provenance()
            .and_then(|g| g.repo)
            .map(|url| normalize_repo(&url))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn
    }

    const TTL: i64 = 1800;

    #[test]
    fn acquire_free_target_then_held_by_other() {
        let c = mem();
        assert_eq!(
            acquire(&c, "o/r", "issue#1", "a:1", None, 1000, TTL).unwrap(),
            Acquire::Acquired
        );
        match acquire(&c, "o/r", "issue#1", "b:2", None, 1001, TTL).unwrap() {
            Acquire::Held { owner, .. } => assert_eq!(owner, "a:1"),
            other => panic!("expected Held, got {other:?}"),
        }
    }

    #[test]
    fn reacquiring_own_live_claim_is_idempotent_and_refreshes_heartbeat() {
        let c = mem();
        acquire(&c, "o/r", "issue#1", "a:1", None, 1000, TTL).unwrap();
        assert_eq!(
            acquire(&c, "o/r", "issue#1", "a:1", None, 1500, TTL).unwrap(),
            Acquire::Acquired
        );
        let hb: i64 = c
            .query_row("SELECT heartbeat_at FROM claims", [], |r| r.get(0))
            .unwrap();
        assert_eq!(hb, 1500, "own re-acquire should refresh heartbeat");
    }

    #[test]
    fn stale_claim_is_stealable_but_fresh_one_is_not() {
        let c = mem();
        acquire(&c, "o/r", "issue#1", "a:1", None, 1000, TTL).unwrap();
        // Well within TTL — cannot steal.
        assert!(matches!(
            acquire(&c, "o/r", "issue#1", "b:2", None, 1000 + 100, TTL).unwrap(),
            Acquire::Held { .. }
        ));
        // Past the TTL — steal succeeds and ownership transfers.
        assert_eq!(
            acquire(&c, "o/r", "issue#1", "b:2", None, 1000 + TTL + 1, TTL).unwrap(),
            Acquire::Acquired
        );
        let owner: String = c
            .query_row("SELECT owner FROM claims", [], |r| r.get(0))
            .unwrap();
        assert_eq!(owner, "b:2");
    }

    #[test]
    fn done_target_is_reacquirable_by_anyone() {
        let c = mem();
        acquire(&c, "o/r", "issue#1", "a:1", None, 1000, TTL).unwrap();
        assert!(done(&c, "o/r", "issue#1", "a:1", 1100).unwrap());
        assert_eq!(
            acquire(&c, "o/r", "issue#1", "b:2", None, 1200, TTL).unwrap(),
            Acquire::Acquired
        );
    }

    #[test]
    fn heartbeat_release_done_are_owner_scoped() {
        let c = mem();
        acquire(&c, "o/r", "issue#1", "a:1", None, 1000, TTL).unwrap();
        assert!(!heartbeat(&c, "o/r", "issue#1", "b:2", 1100).unwrap());
        assert!(!release(&c, "o/r", "issue#1", "b:2").unwrap());
        assert!(!done(&c, "o/r", "issue#1", "b:2", 1100).unwrap());
        assert!(heartbeat(&c, "o/r", "issue#1", "a:1", 1100).unwrap());
        assert!(release(&c, "o/r", "issue#1", "a:1").unwrap());
    }

    #[test]
    fn list_hides_stale_and_done_unless_requested() {
        let c = mem();
        acquire(&c, "o/r", "issue#1", "a:1", None, 1000, TTL).unwrap();
        acquire(&c, "o/r", "issue#2", "a:1", None, 1000, TTL).unwrap();
        done(&c, "o/r", "issue#2", "a:1", 1000).unwrap();
        // At now well past issue#1's TTL, it is stale.
        let now = 1000 + TTL + 5;
        let live = list(&c, Some("o/r"), false, now, TTL).unwrap();
        assert!(live.is_empty(), "stale + done should be hidden: {live:?}");
        let all = list(&c, Some("o/r"), true, now, TTL).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|c| c.target == "issue#1" && c.stale));
    }

    #[test]
    fn list_scopes_by_repo() {
        let c = mem();
        acquire(&c, "o/r1", "issue#1", "a:1", None, 1000, TTL).unwrap();
        acquire(&c, "o/r2", "issue#1", "a:1", None, 1000, TTL).unwrap();
        let r1 = list(&c, Some("o/r1"), true, 1000, TTL).unwrap();
        assert_eq!(r1.len(), 1);
        assert_eq!(r1[0].repo, "o/r1");
        assert_eq!(list(&c, None, true, 1000, TTL).unwrap().len(), 2);
    }

    #[test]
    fn normalize_repo_handles_https_ssh_alias_and_dotgit() {
        assert_eq!(
            normalize_repo("https://github.com/getappz/agentflare.git"),
            "getappz/agentflare"
        );
        assert_eq!(
            normalize_repo("https://github.com/getappz/agentflare"),
            "getappz/agentflare"
        );
        assert_eq!(
            normalize_repo("git@github.com:getappz/agentflare.git"),
            "getappz/agentflare"
        );
        // SSH host alias (this repo's real remote shape).
        assert_eq!(
            normalize_repo("git@github-appzdev:getappz/agentflare.git"),
            "getappz/agentflare"
        );
        assert_eq!(
            normalize_repo("ssh://git@github.com/getappz/agentflare.git"),
            "getappz/agentflare"
        );
    }
}
