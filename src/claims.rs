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
use rusqlite::{Connection, OptionalExtension, params};

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
    )?;
    add_scope_column_if_missing(conn)?;
    add_stop_columns_if_missing(conn)
}

/// Additive migration for installs that created `claims` before the `scope`
/// column existed — unlike `CREATE TABLE IF NOT EXISTS`, `ALTER TABLE ADD
/// COLUMN` isn't naturally idempotent, so this checks first.
fn add_scope_column_if_missing(conn: &Connection) -> rusqlite::Result<()> {
    let has_scope: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('claims') WHERE name = 'scope'")?
        .exists([])?;
    if !has_scope {
        conn.execute("ALTER TABLE claims ADD COLUMN scope TEXT", [])?;
    }
    Ok(())
}

/// Additive migration for installs that created `claims` before the
/// stop-signal columns existed (item #297's cooperative stop mechanism) —
/// same idempotency concern as `add_scope_column_if_missing`.
fn add_stop_columns_if_missing(conn: &Connection) -> rusqlite::Result<()> {
    let has_stop: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('claims') WHERE name = 'stop_requested_at'")?
        .exists([])?;
    if !has_stop {
        conn.execute_batch(
            "ALTER TABLE claims ADD COLUMN stop_requested_at INTEGER;
             ALTER TABLE claims ADD COLUMN stop_reason TEXT;",
        )?;
    }
    Ok(())
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
    /// Path globs this claim's owner declared write ownership over. Empty
    /// (the back-compat default) means "no scope declared" — never used to
    /// deny another agent (see `flare_git_core::scope`).
    pub scope: Vec<String>,
    /// Heartbeat older than the TTL — the claim is effectively available.
    pub stale: bool,
}

/// Attempts to claim `target`. Delegates the atomic acquire/steal upsert to
/// the generic ledger, then — only once we actually hold the claim — stores
/// `git_commit` with a follow-up UPDATE. `git_commit` isn't a column the
/// generic `ClaimLedger` knows about, so it can't be part of the atomic
/// upsert itself; this is a deliberate two-step, not an oversight, and it's
/// safe because the second statement only ever touches a row we just won.
// 8 positional args (one over clippy's default threshold) is still the
// clearest signature here -- every param is self-explanatory, and this
// ledger already has no builder/options-struct precedent elsewhere.
#[allow(clippy::too_many_arguments)]
pub fn acquire(
    conn: &Connection,
    repo: &str,
    target: &str,
    owner: &str,
    git_commit: Option<&str>,
    scope: Option<&[String]>,
    now: i64,
    ttl_secs: i64,
) -> rusqlite::Result<Acquire> {
    let outcome = LEDGER.acquire(conn, &[repo, target], owner, now, ttl_secs)?;
    if outcome == Acquire::Acquired {
        let scope_json = scope.map(|s| serde_json::to_string(s).unwrap_or_default());
        // Scoped to owner: if another owner steals the lease between LEDGER.acquire()
        // and this UPDATE, this must not overwrite their row's provenance with ours.
        // Also clears any pending stop signal -- acquire() (unlike heartbeat(), the
        // repeated-refresh call) marks the start of a claim session, so a stop
        // requested against a prior session shouldn't carry over into a new one.
        conn.execute(
            "UPDATE claims SET git_commit = ?3, scope = ?5, stop_requested_at = NULL, stop_reason = NULL
             WHERE repo = ?1 AND target = ?2 AND owner = ?4",
            params![repo, target, git_commit, owner, scope_json],
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

/// A pending stop request against a live claim, as read back by `should_stop`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StopSignal {
    pub reason: Option<String>,
    pub requested_at: i64,
}

/// Flags a live claim for cooperative stop -- the human-in-the-loop signal
/// half of the `should_stop` poll contract (EPIC #131), modeled on
/// AgentGit's `agt stop <id> --reason` / `agt agent should-stop`. Deliberately
/// NOT owner-scoped, unlike `heartbeat`/`release`/`done`: like
/// `reassignment_releases_claim`, this is a human or coordinator asking the
/// *owner* to stop, so gating it on the caller already being that owner would
/// defeat the purpose. It is honor-system, same as the reference tool's
/// worktree-lock half being optional for v1 -- the owner must actually poll
/// `should_stop` and act on it. Returns false if there is no live claim on
/// `repo`/`target` to signal.
pub fn request_stop(
    conn: &Connection,
    repo: &str,
    target: &str,
    reason: Option<&str>,
    now: i64,
) -> rusqlite::Result<bool> {
    Ok(conn.execute(
        "UPDATE claims SET stop_requested_at = ?3, stop_reason = ?4
         WHERE repo = ?1 AND target = ?2 AND status = 'claimed'",
        params![repo, target, now, reason],
    )? > 0)
}

/// Read-only poll: is a stop currently requested for this claim? Meant to be
/// called from inside an agent's own work loop (mirrors `agt agent
/// should-stop --exit-code`'s 0/1/>1 exit-code contract at the CLI layer).
pub fn should_stop(
    conn: &Connection,
    repo: &str,
    target: &str,
) -> rusqlite::Result<Option<StopSignal>> {
    conn.query_row(
        "SELECT stop_requested_at, stop_reason FROM claims
         WHERE repo = ?1 AND target = ?2 AND stop_requested_at IS NOT NULL",
        params![repo, target],
        |r| {
            Ok(StopSignal {
                requested_at: r.get(0)?,
                reason: r.get(1)?,
            })
        },
    )
    .optional()
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
        "SELECT repo, target, owner, status, created_at, heartbeat_at, git_commit, scope
         FROM claims
         WHERE (?1 IS NULL OR repo = ?1)
         ORDER BY repo, target",
    )?;
    let rows = stmt.query_map(params![repo], |r| {
        let heartbeat_at: i64 = r.get(5)?;
        let status: String = r.get(3)?;
        let scope_json: Option<String> = r.get(7)?;
        Ok(Claim {
            repo: r.get(0)?,
            target: r.get(1)?,
            owner: r.get(2)?,
            stale: status == "claimed" && heartbeat_at < stale_before,
            status,
            created_at: r.get(4)?,
            heartbeat_at,
            git_commit: r.get(6)?,
            scope: scope_json
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
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

/// Best-effort claim-time warning (never blocks -- v1 scope enforcement is
/// at mutation time, see `flare_git_core::scope`): does `scope` overlap a
/// DIFFERENT live claim's declared scope in the same repo?
pub fn scope_overlap_warning(
    conn: &Connection,
    repo: &str,
    target: &str,
    scope: &[String],
    now: i64,
    ttl_secs: i64,
) -> rusqlite::Result<Option<String>> {
    if flare_git_core::scope::scope_is_wildcard_or_empty(scope) {
        return Ok(None);
    }
    let others = list(conn, Some(repo), false, now, ttl_secs)?;
    for other in others {
        if other.target == target || flare_git_core::scope::scope_is_wildcard_or_empty(&other.scope)
        {
            continue;
        }
        if scope.iter().any(|a| {
            other
                .scope
                .iter()
                .any(|b| flare_git_core::scope::globs_overlap(a, b))
        }) {
            return Ok(Some(format!(
                "scope overlaps live claim '{}' (owner {}, scope {:?})",
                other.target, other.owner, other.scope
            )));
        }
    }
    Ok(None)
}

/// Best-effort claim-time warning: does re-acquiring with `new_scope` clear
/// an existing, previously-declared non-empty scope? `acquire()` always
/// overwrites the `scope` column (mirroring `git_commit`'s always-overwrite
/// behavior), so a caller re-acquiring an already-held claim without
/// re-supplying `--scope`/`scope` silently disables path-scope enforcement
/// for it -- read BEFORE calling `acquire()` so this reflects the row's
/// state prior to the overwrite.
pub fn scope_clear_warning(
    conn: &Connection,
    repo: &str,
    target: &str,
    new_scope: Option<&[String]>,
) -> rusqlite::Result<Option<String>> {
    if new_scope.is_some_and(|s| !flare_git_core::scope::scope_is_wildcard_or_empty(s)) {
        return Ok(None); // caller is declaring a real scope -- nothing being cleared
    }
    let existing: Option<String> = conn
        .query_row(
            "SELECT scope FROM claims WHERE repo = ?1 AND target = ?2",
            params![repo, target],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let existing_scope: Vec<String> = existing
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if flare_git_core::scope::scope_is_wildcard_or_empty(&existing_scope) {
        return Ok(None);
    }
    Ok(Some(format!(
        "re-acquiring without --scope clears the existing scope {existing_scope:?} -- pass --scope again to keep enforcement active"
    )))
}

// --- identity / config resolution (impure; thin wrappers over env + git) ---

std::thread_local! {
    // Per-thread override for `owner_id()`. `AGENTFLARE_AGENT`/`AGENTFLARE_SESSION`
    // are process-global, which is fine for a fresh-process-per-command CLI
    // but wrong once multiple work items run as threads inside one long-lived
    // daemon process (see `with_owner_override`'s doc comment) — two worker
    // threads racing on `std::env::set_var` could attribute a claim/comment
    // to the wrong agent. Thread-local sidesteps that: each worker thread's
    // override is independent, no shared mutable state, no locking needed.
    static OWNER_OVERRIDE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Runs `f` with `owner_id()` returning `owner` instead of resolving it from
/// env — for in-process job execution (`agentflare_jobs::InProcessExecutor`),
/// where every worker thread shares the daemon's one process env/pid and so
/// can't rely on `AGENTFLARE_AGENT`/`AGENTFLARE_SESSION`/pid the way a fresh
/// `agentflare work` subprocess naturally can. `owner` should already be the
/// full `<agent>:<instance>` pair (e.g. `claude-code:<job-id>`) — the job's
/// own queue id makes a good instance discriminator, playing the same role
/// the subprocess's own unique pid plays today.
pub fn with_owner_override<R>(owner: impl Into<String>, f: impl FnOnce() -> R) -> R {
    struct ClearOnDrop;
    impl Drop for ClearOnDrop {
        fn drop(&mut self) {
            OWNER_OVERRIDE.with(|cell| *cell.borrow_mut() = None);
        }
    }
    OWNER_OVERRIDE.with(|cell| *cell.borrow_mut() = Some(owner.into()));
    let _clear = ClearOnDrop;
    f()
}

/// Whether the calling thread is currently inside a [`with_owner_override`]
/// scope — lets callers that would otherwise mutate the process-global
/// `AGENTFLARE_AGENT` env var (safe only when there's a single process per
/// identity) skip that mutation once identity is already established
/// per-thread instead.
pub fn has_owner_override() -> bool {
    OWNER_OVERRIDE.with(|cell| cell.borrow().is_some())
}

/// `<agent>:<instance>` — same agent chain as handoff, plus an instance
/// discriminator so two parallel sessions of one agent are distinct owners.
///
/// Instance is `AGENTFLARE_SESSION` if set, else a per-process id
/// (`process_instance_id`: pid plus a random suffix). A long-lived MCP
/// server computes it once, so all its `claim_*` calls share one owner —
/// the common case. The CLI, however, is a fresh process per command, so
/// `AGENTFLARE_SESSION` must be set to keep ownership continuous across
/// separate `agentflare claim` invocations (acquire in one, release in
/// another); otherwise each command is a distinct owner.
///
/// `AGENTFLARE_CLAIM_OWNER`, when set, is read verbatim ahead of the
/// agent/instance reconstruction below — the cross-process counterpart to
/// `OWNER_OVERRIDE` for a dispatched subprocess (`agent_launch::run_headless_
/// with_owner`) whose own execution agent (`AGENTFLARE_AGENT`, needed intact
/// for `flare-git-shim`'s bypass classification) can differ from the claim
/// owner that dispatched it (item #538) — splicing the owner's agent into
/// `AGENTFLARE_AGENT` instead would let that bypass check target the wrong
/// agent.
pub fn owner_id() -> String {
    if let Some(owner) = OWNER_OVERRIDE.with(|cell| cell.borrow().clone()) {
        return owner;
    }
    if let Some(owner) = std::env::var("AGENTFLARE_CLAIM_OWNER")
        .ok()
        .filter(|s| !s.is_empty())
    {
        return owner;
    }
    let agent = std::env::var("AGENTFLARE_AGENT")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(agent_detector::agent_name)
        .unwrap_or_else(|| "cli".to_string());
    let instance = std::env::var("AGENTFLARE_SESSION")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| process_instance_id().to_string());
    format!("{agent}:{instance}")
}

/// This process's owner-instance discriminator: `<pid>-<random>`, computed
/// once. A bare pid is not unique enough — every sandboxed job run under
/// bwrap's `--unshare-pid` sees itself as a tiny pid (often the same one),
/// and pids repeat across machines sharing a synced db — and `acquire`
/// treats an identical owner string as "already ours", so two such
/// processes would silently share one claim. The pid stays as a prefix for
/// readability; no `:` so `agent_of`/`agent_part` still split correctly.
fn process_instance_id() -> &'static str {
    static INSTANCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    INSTANCE.get_or_init(|| format!("{}-{:016x}", std::process::id(), rand::random::<u64>()))
}

/// Strips the `:<instance>` suffix off an owner id, leaving the stable agent
/// identity. Unlike claim ownership (deliberately instance-scoped, see
/// `owner_id` above), authorship of a comment should survive across
/// sessions — an agent restarting shouldn't lose the ability to edit its own
/// words just because its instance suffix changed.
pub fn agent_of(owner_id: &str) -> &str {
    owner_id.split(':').next().unwrap_or(owner_id)
}

/// Default lease (30 min, `AGENTFLARE_CLAIM_TTL_SECS` overrides): a claim
/// whose owner hasn't heartbeat within this window is stealable, so a
/// crashed/hung agent can't wedge a target forever. Same value the backend
/// crate applies to item claims.
pub fn ttl_secs() -> i64 {
    agentflare_backend::claim::default_ttl_secs()
}

/// When an item's `assignee_agent` is updated to a different agent, release
/// any existing claim on that item held by the old agent. Uses `agent_of()`
/// to compare agent parts (ignoring `:<instance>` suffix), so a same-agent
/// re-assignment (e.g. instance refresh) does NOT release the claim.
///
/// Deliberately NOT authz-gated on caller identity: agentflare is local-only and
/// cooperative (see SECURITY.md), so a reassignment is an intentional hand-off —
/// the coordinator or new assignee, not necessarily the current owner, may trigger
/// the release. An owner-only gate here would break legitimate hand-offs.
pub fn reassignment_releases_claim(
    conn: &rusqlite::Connection,
    item_id: &str,
    new_assignee: Option<&str>,
) -> rusqlite::Result<bool> {
    let Some(owner) = agentflare_backend::claim::current_owner(conn, item_id) else {
        return Ok(false);
    };
    let Some(new) = new_assignee else {
        return Ok(false);
    };
    if agent_of(&owner) == agent_of(new) {
        return Ok(false);
    }
    agentflare_backend::claim::release(conn, item_id, &owner)?;
    Ok(true)
}

/// The job-queue side of a reassignment: cancels `item_id`'s queued/retrying
/// and running in-process jobs that target a different agent than
/// `new_assignee`, so the old agent's job can't keep working (or open a PR)
/// alongside the new one, and its stale queue entry doesn't hold back the
/// fresh dispatch. Same-agent reassignment (instance refresh) cancels nothing.
pub fn reassignment_cancels_jobs(
    queue: &agentflare_jobs::Queue,
    item_id: &str,
    new_assignee: &str,
) -> Result<Vec<String>, agentflare_jobs::queue::Error> {
    let new_agent = agentflare_backend::item::agent_part(new_assignee);
    queue.cancel_for_item(item_id, |job_agent| {
        agentflare_backend::item::agent_part(job_agent) == new_agent
    })
}

pub fn now() -> i64 {
    db_kit::ids::now()
}

/// Normalizes a git remote URL to a stable `owner/name` key, so https and ssh
/// forms of the same repo map to one claim namespace.
/// `https://github.com/getappz/agentflare.git` and
/// `git@github-alias:getappz/agentflare.git` both → `getappz/agentflare`.
pub use crate::github::identity::normalize_repo;

/// Maps a git remote URL to its `owner/name` claim key, enforcing the issue
/// #224 gate: only a confirmed GitHub origin resolves (see `RepoId::parse`).
/// A GitLab/Bitbucket origin returns `None`, so callers must require an
/// explicit `--repo`.
fn repo_key_from_url(url: &str) -> Option<String> {
    crate::github::identity::RepoId::parse(url).map(|r| r.to_string())
}

/// Resolves the repo key: explicit `--repo` wins, else resolve the origin
/// remote from git provenance. Routing through `RepoId::parse` enforces the
/// issue #224 gate — a non-GitHub origin (GitLab/Bitbucket) returns `None`
/// here, forcing the caller to pass an explicit `--repo`.
pub fn resolve_repo(explicit: Option<String>) -> Option<String> {
    explicit.filter(|s| !s.is_empty()).or_else(|| {
        crate::mcp_server::AgentflareMcp::git_provenance()
            .and_then(|g| g.repo)
            .as_deref()
            .and_then(repo_key_from_url)
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
    fn list_hides_stale_and_done_unless_requested() {
        let c = mem();
        acquire(&c, "o/r", "issue#1", "a:1", None, None, 1000, TTL).unwrap();
        acquire(&c, "o/r", "issue#2", "a:1", None, None, 1000, TTL).unwrap();
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
        acquire(&c, "o/r1", "issue#1", "a:1", None, None, 1000, TTL).unwrap();
        acquire(&c, "o/r2", "issue#1", "a:1", None, None, 1000, TTL).unwrap();
        let r1 = list(&c, Some("o/r1"), true, 1000, TTL).unwrap();
        assert_eq!(r1.len(), 1);
        assert_eq!(r1[0].repo, "o/r1");
        assert_eq!(list(&c, None, true, 1000, TTL).unwrap().len(), 2);
    }

    #[test]
    fn acquire_persists_and_overwrites_scope() {
        let c = mem();
        let scope = vec!["crates/foo/".to_string()];
        acquire(&c, "o/r", "issue#1", "a:1", None, Some(&scope), 1000, TTL).unwrap();
        let claims = list(&c, Some("o/r"), true, 1000, TTL).unwrap();
        assert_eq!(claims[0].scope, scope);

        // Re-acquiring with no scope overwrites it back to unscoped, mirroring
        // git_commit's always-overwrite behavior.
        acquire(&c, "o/r", "issue#1", "a:1", None, None, 1000, TTL).unwrap();
        let claims = list(&c, Some("o/r"), true, 1000, TTL).unwrap();
        assert!(claims[0].scope.is_empty());
    }

    #[test]
    fn scope_clear_warning_fires_only_when_clearing_a_real_existing_scope() {
        let c = mem();
        let scope = vec!["crates/foo/".to_string()];

        // No existing claim yet -- nothing to clear.
        assert!(
            scope_clear_warning(&c, "o/r", "issue#1", None)
                .unwrap()
                .is_none()
        );

        acquire(&c, "o/r", "issue#1", "a:1", None, Some(&scope), 1000, TTL).unwrap();

        // Re-declaring a real scope isn't a clear.
        let other_scope = vec!["crates/bar/".to_string()];
        assert!(
            scope_clear_warning(&c, "o/r", "issue#1", Some(&other_scope))
                .unwrap()
                .is_none()
        );

        // Re-acquiring with no scope WOULD clear the existing one -- warn.
        let warning = scope_clear_warning(&c, "o/r", "issue#1", None)
            .unwrap()
            .expect("clearing a declared scope must warn");
        assert!(warning.contains("crates/foo/"), "{warning}");

        // Once cleared, re-checking with no scope is a no-op (nothing left to clear).
        acquire(&c, "o/r", "issue#1", "a:1", None, None, 1000, TTL).unwrap();
        assert!(
            scope_clear_warning(&c, "o/r", "issue#1", None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn list_defaults_missing_scope_to_empty() {
        let c = mem();
        acquire(&c, "o/r", "issue#1", "a:1", None, None, 1000, TTL).unwrap();
        let claims = list(&c, Some("o/r"), true, 1000, TTL).unwrap();
        assert!(claims[0].scope.is_empty());
    }

    #[test]
    fn should_stop_is_none_until_requested_then_some_with_reason() {
        let c = mem();
        acquire(&c, "o/r", "issue#1", "a:1", None, None, 1000, TTL).unwrap();
        assert!(should_stop(&c, "o/r", "issue#1").unwrap().is_none());

        assert!(request_stop(&c, "o/r", "issue#1", Some("pausing for review"), 1500).unwrap());
        let signal = should_stop(&c, "o/r", "issue#1").unwrap().unwrap();
        assert_eq!(signal.reason.as_deref(), Some("pausing for review"));
        assert_eq!(signal.requested_at, 1500);
    }

    #[test]
    fn request_stop_returns_false_when_no_live_claim() {
        let c = mem();
        assert!(!request_stop(&c, "o/r", "issue#1", None, 1000).unwrap());

        acquire(&c, "o/r", "issue#1", "a:1", None, None, 1000, TTL).unwrap();
        done(&c, "o/r", "issue#1", "a:1", 1000).unwrap();
        assert!(
            !request_stop(&c, "o/r", "issue#1", None, 1100).unwrap(),
            "a done claim is no longer live -- nothing to signal"
        );
    }

    #[test]
    fn reacquiring_clears_a_stale_stop_signal_from_a_prior_session() {
        let c = mem();
        acquire(&c, "o/r", "issue#1", "a:1", None, None, 1000, TTL).unwrap();
        request_stop(&c, "o/r", "issue#1", Some("stop"), 1100).unwrap();
        assert!(should_stop(&c, "o/r", "issue#1").unwrap().is_some());

        // Same owner re-acquiring (e.g. re-running `claim acquire`) starts a
        // fresh session -- the old stop signal shouldn't silently persist.
        acquire(&c, "o/r", "issue#1", "a:1", None, None, 1200, TTL).unwrap();
        assert!(should_stop(&c, "o/r", "issue#1").unwrap().is_none());
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

    #[test]
    fn repo_key_from_url_accepts_github_and_rejects_non_github() {
        // Issue #224: the claim-namespace resolution must reject non-GitHub
        // origins (which would otherwise collide with a same-named GitHub repo
        // and target GitHub write ops with a GitHub token).
        assert_eq!(
            repo_key_from_url("https://github.com/getappz/agentflare.git"),
            Some("getappz/agentflare".to_string())
        );
        assert_eq!(
            repo_key_from_url("git@github.com:getappz/agentflare.git"),
            Some("getappz/agentflare".to_string())
        );
        assert!(repo_key_from_url("https://bitbucket.org/o/r").is_none());
        assert!(repo_key_from_url("git@gitlab.com:o/r.git").is_none());
        assert!(repo_key_from_url("ssh://git@gitlab.com/o/r.git").is_none());
    }

    #[test]
    fn reassignment_cancels_only_the_old_agents_jobs_for_that_item() {
        let q = agentflare_jobs::Queue::open_memory(std::env::temp_dir()).unwrap();
        let job = |item: &str, agent: &str| {
            q.enqueue(
                &agentflare_jobs::AgentJob::new("agentflare-work")
                    .args([item, agent])
                    .in_process(),
            )
            .unwrap()
            .id
        };
        let old = job("i1", "opencode");
        let refreshed = job("i1", "claude-code");
        let other = job("i2", "opencode");

        // `claude-code:<instance>` is the same agent as the payload's bare name.
        let ids = reassignment_cancels_jobs(&q, "i1", "claude-code:abc").unwrap();

        assert_eq!(ids, vec![old.clone()]);
        assert!(q.is_cancelled(&old));
        assert!(!q.is_cancelled(&refreshed));
        assert!(!q.is_cancelled(&other));
    }

    #[test]
    fn with_owner_override_makes_owner_id_return_the_given_value() {
        let seen = with_owner_override("claude-code:job-123", owner_id);
        assert_eq!(seen, "claude-code:job-123");
    }

    #[test]
    fn owner_id_falls_back_to_env_once_the_override_scope_ends() {
        // Without an override, owner_id() reads env/pid as usual — just
        // asserting the override doesn't leak past its own scope (a stale
        // leftover would misattribute every subsequent claim/comment on this
        // thread to the wrong agent).
        let overridden = with_owner_override("claude-code:job-123", owner_id);
        let after = owner_id();
        assert_eq!(overridden, "claude-code:job-123");
        assert_ne!(after, "claude-code:job-123");
    }

    #[test]
    fn process_instance_id_is_stable_pid_prefixed_and_colon_free() {
        let id = process_instance_id();
        assert_eq!(
            id,
            process_instance_id(),
            "must be computed once per process"
        );
        assert!(id.starts_with(&format!("{}-", std::process::id())), "{id}");
        assert!(!id.contains(':'), "{id}");
        // Longer than the bare pid: carries the random disambiguator.
        assert!(id.len() > std::process::id().to_string().len() + 1, "{id}");
        assert_eq!(agent_of(&format!("claude-code:{id}")), "claude-code");
    }

    #[test]
    fn has_owner_override_reflects_whether_a_scope_is_active() {
        assert!(!has_owner_override());
        with_owner_override("codex:job-9", || {
            assert!(has_owner_override());
        });
        assert!(!has_owner_override());
    }

    // The whole point of a thread-local override (vs. the process-global
    // `AGENTFLARE_AGENT` env var it replaces for in-process job dispatch) is
    // that concurrent worker threads each running a different job's agent
    // never see each other's identity — proves that directly rather than
    // just trusting thread_local!'s documented semantics.
    #[test]
    fn concurrent_overrides_on_different_threads_never_see_each_others_value() {
        let handles: Vec<_> = (0..8)
            .map(|i| {
                std::thread::spawn(move || {
                    let owner = format!("agent-{i}:job-{i}");
                    with_owner_override(owner.clone(), || {
                        // Yield repeatedly so other threads' set/clear cycles
                        // have every chance to interleave with this one if
                        // the override were shared instead of thread-local.
                        for _ in 0..50 {
                            assert_eq!(owner_id(), owner);
                            std::thread::yield_now();
                        }
                    });
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }
}
