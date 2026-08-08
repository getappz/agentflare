//! Cross-workstation memory sync over a shared git branch. `brain.db` is
//! pure local SQLite with no sync of its own; this exports `observations` as
//! JSONL, merges it against the same file on a dedicated GitHub branch (the
//! newer `updated_at` per fact wins), imports the merged result back into
//! this machine, and pushes the merge if it changed anything remotely.
//!
//! Deliberately app-level (not a `brain.db` file sync): `observations` is an
//! accumulating log, and two machines writing between syncs is the normal
//! case, not a rare edge case -- a whole-file last-write-wins would silently
//! drop whichever side didn't win.

use crate::github::{Client, RepoId};
use crate::memory::observations::{self, Observation, SaveOutcome};
use rusqlite::Connection;

const DEFAULT_BRANCH: &str = "agentflare-memory";
const DEFAULT_PATH: &str = "memory-sync.jsonl";

#[derive(Debug)]
pub struct MemorySyncConfig {
    pub repo: RepoId,
    pub branch: String,
    pub path: String,
}

impl MemorySyncConfig {
    /// Repo defaults to the current directory's `origin` remote (same as
    /// the GitHub bridge), so no per-machine setup is needed and each
    /// project's observations land on its own branch of its own repo
    /// rather than one global log shared across every project on this
    /// workstation. Override with `AGENTFLARE_MEMORY_SYNC_REPO`
    /// (`owner/repo`) to point somewhere else.
    /// `AGENTFLARE_MEMORY_SYNC_BRANCH` (default `agentflare-memory`) and
    /// `AGENTFLARE_MEMORY_SYNC_PATH` (default `memory-sync.jsonl`) are
    /// still independently overridable.
    pub fn from_env() -> Result<MemorySyncConfig, String> {
        let cwd = std::env::current_dir()
            .map_err(|e| format!("cannot read the working directory: {e}"))?;
        Self::from_env_at(&cwd)
    }

    /// Split out from `from_env` so repo resolution is testable without
    /// mutating the process's working directory.
    pub fn from_env_at(repo_root: &std::path::Path) -> Result<MemorySyncConfig, String> {
        let repo = match std::env::var("AGENTFLARE_MEMORY_SYNC_REPO")
            .ok()
            .filter(|s| !s.trim().is_empty())
        {
            Some(repo_str) => RepoId::parse(repo_str.trim()).ok_or_else(|| {
                format!("AGENTFLARE_MEMORY_SYNC_REPO={repo_str:?} is not a GitHub owner/repo")
            })?,
            None => RepoId::resolve_from_remote(repo_root).ok_or_else(|| {
                "no GitHub `origin` remote here -- run this from a repo you can push to, \
                 or set AGENTFLARE_MEMORY_SYNC_REPO=owner/repo"
                    .to_string()
            })?,
        };
        let branch = std::env::var("AGENTFLARE_MEMORY_SYNC_BRANCH")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_BRANCH.to_string());
        let path = std::env::var("AGENTFLARE_MEMORY_SYNC_PATH")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_PATH.to_string());
        Ok(MemorySyncConfig { repo, branch, path })
    }
}

/// One observation's wire representation. Deliberately narrower than
/// `Observation`: `id`/`session_id`/`tool_name`/`revision_count`/
/// `duplicate_count`/`last_seen_at`/`review_after`/`pinned`/`deleted_at` are
/// this machine's local bookkeeping, not facts to carry across the wire.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SyncEntry {
    pub hash: String,
    pub topic_key: Option<String>,
    pub r#type: String,
    pub title: String,
    pub content: String,
    pub project: Option<String>,
    pub scope: String,
    pub created_at: String,
    pub updated_at: String,
}

impl From<&Observation> for SyncEntry {
    fn from(o: &Observation) -> SyncEntry {
        SyncEntry {
            hash: o
                .normalized_hash
                .clone()
                .unwrap_or_else(|| observations::hash_normalized(&o.title, &o.content)),
            topic_key: o.topic_key.clone(),
            r#type: o.r#type.clone(),
            title: o.title.clone(),
            content: o.content.clone(),
            project: o.project.clone(),
            scope: o.scope.clone(),
            created_at: o.created_at.clone(),
            updated_at: o.updated_at.clone(),
        }
    }
}

pub fn export_all(conn: &Connection) -> rusqlite::Result<Vec<SyncEntry>> {
    Ok(observations::list_all(conn)?
        .iter()
        .map(SyncEntry::from)
        .collect())
}

fn merge_key(e: &SyncEntry) -> (String, String) {
    (
        e.project.clone().unwrap_or_default(),
        e.topic_key.clone().unwrap_or_else(|| e.hash.clone()),
    )
}

/// Unions `remote` and `local`, keeping the entry with the later
/// `updated_at` (ties keep whichever was seen first) wherever they collide
/// on `(project, topic_key-or-hash)`. Pure and side-effect free, so this is
/// the same merge whether it feeds an import into the local db or a push
/// back to the remote branch -- both sides converge on this exact result.
pub fn merge(remote: Vec<SyncEntry>, local: Vec<SyncEntry>) -> Vec<SyncEntry> {
    let mut merged: std::collections::BTreeMap<(String, String), SyncEntry> =
        std::collections::BTreeMap::new();
    for e in remote.into_iter().chain(local) {
        let key = merge_key(&e);
        match merged.get(&key) {
            Some(existing) if existing.updated_at >= e.updated_at => {}
            _ => {
                merged.insert(key, e);
            }
        }
    }
    let mut out: Vec<SyncEntry> = merged.into_values().collect();
    out.sort_by(|a, b| {
        (a.project.as_deref().unwrap_or(""), a.created_at.as_str())
            .cmp(&(b.project.as_deref().unwrap_or(""), b.created_at.as_str()))
    });
    out
}

/// Imports `entries` into the local db via `observations::upsert_synced`,
/// which -- unlike `save` -- refuses to let a stale remote row clobber a
/// fresher local edit. Returns how many entries actually created or updated
/// a row (excludes ones already current locally).
pub fn import_entries(conn: &Connection, entries: &[SyncEntry]) -> rusqlite::Result<usize> {
    let mut changed = 0;
    for e in entries {
        let outcome = observations::upsert_synced(
            conn,
            &e.r#type,
            &e.title,
            &e.content,
            e.project.as_deref(),
            &e.scope,
            e.topic_key.as_deref(),
            &e.hash,
            &e.created_at,
            &e.updated_at,
        )?;
        if !matches!(outcome, SaveOutcome::Duplicate(_)) {
            changed += 1;
        }
    }
    Ok(changed)
}

/// Parses every nonblank line as a `SyncEntry`, or fails on the first bad
/// one. Silently dropping unparseable lines (the old behavior) would mean a
/// single malformed record permanently deletes itself from the shared log
/// the next time this machine pushes a merge that omits it -- better to
/// abort the whole sync than to quietly rewrite the file out from under
/// whoever wrote that line.
fn parse_jsonl(text: &str) -> Result<Vec<SyncEntry>, String> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| {
            serde_json::from_str(l).map_err(|e| format!("invalid JSONL at line {}: {e}", i + 1))
        })
        .collect()
}

fn to_jsonl(entries: &[SyncEntry]) -> String {
    let mut out = String::new();
    for e in entries {
        out.push_str(&serde_json::to_string(e).unwrap_or_default());
        out.push('\n');
    }
    out
}

pub struct SyncReport {
    pub imported: usize,
    pub pushed: bool,
}

/// One push+pull cycle: fetch the remote log, merge it with local
/// observations (newer `updated_at` per fact wins either way), import the
/// merge locally, and push it back if it changed the remote file. A single
/// retry on a 409 (a concurrent writer beat us to the sha) refetches and
/// retries once -- a second collision is left to the next scheduled sync
/// rather than looped on indefinitely.
pub fn run(
    conn: &Connection,
    client: &Client,
    config: &MemorySyncConfig,
) -> Result<SyncReport, String> {
    crate::github::contents::ensure_branch(client, &config.repo, &config.branch)
        .map_err(|e| format!("ensure branch {}: {e}", config.branch))?;

    let mut existing =
        crate::github::contents::get_file(client, &config.repo, &config.path, &config.branch)
            .map_err(|e| format!("fetch remote memory log: {e}"))?;

    let remote_entries = existing
        .as_ref()
        .map(|f| parse_jsonl(&f.content))
        .transpose()
        .map_err(|e| format!("parse remote memory log: {e}"))?
        .unwrap_or_default();
    let local_entries = export_all(conn).map_err(|e| format!("read local observations: {e}"))?;
    let merged = merge(remote_entries, local_entries);

    let mut imported =
        import_entries(conn, &merged).map_err(|e| format!("import merged entries: {e}"))?;

    let serialized = to_jsonl(&merged);
    let unchanged = existing.as_ref().is_some_and(|f| f.content == serialized);
    let pushed = if unchanged {
        false
    } else {
        let sha = existing.as_ref().map(|f| f.sha.clone());
        let result = crate::github::contents::put_file(
            client,
            &config.repo,
            &config.path,
            &config.branch,
            "chore: sync memory observations",
            &serialized,
            sha.as_deref(),
        );
        match result {
            Ok(_) => true,
            Err(crate::github::GitHubError::Http { status: 409, .. }) => {
                // Someone else pushed between our GET and PUT. Refetch and
                // re-merge from scratch rather than blindly retrying the
                // stale `serialized` payload -- that would force-push over
                // whatever the other workstation just added, which is
                // exactly the loss this whole module exists to prevent.
                existing = crate::github::contents::get_file(
                    client,
                    &config.repo,
                    &config.path,
                    &config.branch,
                )
                .map_err(|e| format!("refetch after conflict: {e}"))?;

                let retry_remote = existing
                    .as_ref()
                    .map(|f| parse_jsonl(&f.content))
                    .transpose()
                    .map_err(|e| format!("parse remote memory log after conflict: {e}"))?
                    .unwrap_or_default();
                let retry_merged = merge(
                    retry_remote,
                    export_all(conn).map_err(|e| format!("read local observations: {e}"))?,
                );
                imported += import_entries(conn, &retry_merged)
                    .map_err(|e| format!("import retry merge: {e}"))?;
                let retry_serialized = to_jsonl(&retry_merged);

                crate::github::contents::put_file(
                    client,
                    &config.repo,
                    &config.path,
                    &config.branch,
                    "chore: sync memory observations",
                    &retry_serialized,
                    existing.as_ref().map(|f| f.sha.as_str()),
                )
                .map_err(|e| format!("push merged memory log after retry: {e}"))?;
                true
            }
            Err(e) => return Err(format!("push merged memory log: {e}")),
        }
    };

    Ok(SyncReport { imported, pushed })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(project: &str, topic_key: &str, title: &str, updated_at: &str) -> SyncEntry {
        SyncEntry {
            hash: observations::hash_normalized(title, title),
            topic_key: Some(topic_key.to_string()),
            r#type: "decision".to_string(),
            title: title.to_string(),
            content: title.to_string(),
            project: Some(project.to_string()),
            scope: "project".to_string(),
            created_at: updated_at.to_string(),
            updated_at: updated_at.to_string(),
        }
    }

    #[test]
    fn merge_keeps_the_newer_side_on_a_topic_key_collision() {
        let remote = vec![entry("p", "topic-x", "old", "2026-01-01T00:00:00.000Z")];
        let local = vec![entry("p", "topic-x", "new", "2026-06-01T00:00:00.000Z")];
        let merged = merge(remote, local);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].title, "new");
    }

    #[test]
    fn merge_prefers_local_when_local_is_newer_and_remote_when_remote_is_newer() {
        let remote = vec![entry(
            "p",
            "topic-x",
            "remote-newer",
            "2026-06-01T00:00:00.000Z",
        )];
        let local = vec![entry(
            "p",
            "topic-x",
            "local-older",
            "2026-01-01T00:00:00.000Z",
        )];
        let merged = merge(remote, local);
        assert_eq!(merged[0].title, "remote-newer");
    }

    #[test]
    fn merge_keeps_disjoint_entries_from_both_sides() {
        let remote = vec![entry("p", "topic-a", "a", "2026-01-01T00:00:00.000Z")];
        let local = vec![entry("p", "topic-b", "b", "2026-01-01T00:00:00.000Z")];
        let merged = merge(remote, local);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merge_scopes_topic_key_collisions_by_project() {
        let remote = vec![entry("proj-a", "topic-x", "a", "2026-01-01T00:00:00.000Z")];
        let local = vec![entry("proj-b", "topic-x", "b", "2026-01-01T00:00:00.000Z")];
        let merged = merge(remote, local);
        assert_eq!(
            merged.len(),
            2,
            "same topic_key in different projects must not collapse"
        );
    }

    #[test]
    fn jsonl_roundtrips_through_parse_and_serialize() {
        let entries = vec![
            entry("p", "topic-a", "a", "2026-01-01T00:00:00.000Z"),
            entry("p", "topic-b", "b", "2026-01-02T00:00:00.000Z"),
        ];
        let text = to_jsonl(&entries);
        assert_eq!(parse_jsonl(&text).unwrap(), entries);
    }

    #[test]
    fn parse_jsonl_fails_loud_on_a_malformed_line_instead_of_dropping_it() {
        let text = "{\"not\":\"a valid SyncEntry\"}\n";
        let err = parse_jsonl(text).unwrap_err();
        assert!(
            err.contains("line 1"),
            "error should name the offending line: {err}"
        );
    }

    #[test]
    fn from_env_at_falls_back_to_the_repo_roots_origin_remote() {
        let _guard = agent_registry::detect::PATH_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original = std::env::var_os("AGENTFLARE_MEMORY_SYNC_REPO");
        unsafe {
            std::env::remove_var("AGENTFLARE_MEMORY_SYNC_REPO");
        }

        let dir = tempfile::tempdir().unwrap();
        flare_git_core::shell::run_in(dir.path(), &["init", "-q"]).unwrap();
        flare_git_core::shell::run_in(
            dir.path(),
            &["remote", "add", "origin", "git@github.com:o/r.git"],
        )
        .unwrap();
        let config = MemorySyncConfig::from_env_at(dir.path()).unwrap();
        assert_eq!(config.repo.to_string(), "o/r");

        if let Some(v) = original {
            unsafe {
                std::env::set_var("AGENTFLARE_MEMORY_SYNC_REPO", v);
            }
        }
    }

    #[test]
    fn from_env_at_requires_a_repo_when_theres_no_origin_remote() {
        let _guard = agent_registry::detect::PATH_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original = std::env::var_os("AGENTFLARE_MEMORY_SYNC_REPO");
        unsafe {
            std::env::remove_var("AGENTFLARE_MEMORY_SYNC_REPO");
        }

        let dir = tempfile::tempdir().unwrap();
        let err = MemorySyncConfig::from_env_at(dir.path()).unwrap_err();
        assert!(err.contains("AGENTFLARE_MEMORY_SYNC_REPO"));

        if let Some(v) = original {
            unsafe {
                std::env::set_var("AGENTFLARE_MEMORY_SYNC_REPO", v);
            }
        }
    }

    use crate::github::test_support::{MockResponse, MockServer};
    use crate::memory::schema;

    fn new_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        schema::migrate(&mut conn).unwrap();
        conn
    }

    fn config() -> MemorySyncConfig {
        MemorySyncConfig {
            repo: RepoId {
                owner: "o".into(),
                repo: "r".into(),
            },
            branch: "agentflare-memory".into(),
            path: "memory-sync.jsonl".into(),
        }
    }

    #[test]
    fn run_pulls_a_remote_only_entry_and_pushes_the_merged_local_one() {
        let conn = new_db();
        observations::save(
            &conn,
            None,
            "decision",
            "local fact",
            "local content",
            None,
            Some("proj-a"),
            None,
            Some("topic-local"),
        )
        .unwrap();

        let remote_entry = entry(
            "proj-a",
            "topic-remote",
            "remote fact",
            "2026-01-01T00:00:00.000Z",
        );
        let remote_jsonl = to_jsonl(&[remote_entry]);
        let b64 = {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(remote_jsonl.as_bytes())
        };

        let server = MockServer::start(vec![
            // ensure_branch: branch already exists.
            MockResponse::json(200, r#"{"object":{"sha":"tip"}}"#),
            // get_file: current remote content.
            MockResponse::json(
                200,
                &format!(r#"{{"sha":"filesha","content":"{b64}","encoding":"base64"}}"#),
            ),
            // put_file: push the merged result.
            MockResponse::json(200, r#"{"content":{"sha":"newsha"}}"#),
        ]);
        let client = server.client(Some("tok"));

        let report = run(&conn, &client, &config()).unwrap();
        assert!(report.pushed);
        assert_eq!(
            report.imported, 1,
            "the remote-only entry should be imported"
        );

        let all = observations::list_all(&conn).unwrap();
        assert!(all.iter().any(|o| o.title == "remote fact"));
        assert!(all.iter().any(|o| o.title == "local fact"));

        let reqs = server.requests();
        assert_eq!(reqs[2].method, "PUT");
        let sent: serde_json::Value = serde_json::from_str(&reqs[2].body).unwrap();
        assert_eq!(sent["branch"], "agentflare-memory");
    }

    #[test]
    fn run_skips_the_push_when_the_merge_matches_remote_exactly() {
        let conn = new_db();
        let remote_entry = entry(
            "proj-a",
            "topic-x",
            "shared fact",
            "2026-01-01T00:00:00.000Z",
        );
        let remote_jsonl = to_jsonl(&[remote_entry]);
        let b64 = {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(remote_jsonl.as_bytes())
        };

        let server = MockServer::start(vec![
            MockResponse::json(200, r#"{"object":{"sha":"tip"}}"#),
            MockResponse::json(
                200,
                &format!(r#"{{"sha":"filesha","content":"{b64}","encoding":"base64"}}"#),
            ),
        ]);
        let client = server.client(Some("tok"));

        let report = run(&conn, &client, &config()).unwrap();
        assert!(!report.pushed, "identical merge must not trigger a PUT");
        let reqs = server.requests();
        assert_eq!(reqs.len(), 2, "no PUT request should have been made");
    }

    #[test]
    fn run_re_merges_after_a_409_conflict_instead_of_overwriting_a_concurrent_push() {
        use base64::Engine as _;

        let conn = new_db();
        observations::save(
            &conn,
            None,
            "decision",
            "local fact",
            "local content",
            None,
            Some("proj-a"),
            None,
            Some("topic-local"),
        )
        .unwrap();

        let remote_entry = entry(
            "proj-a",
            "topic-x",
            "remote fact",
            "2026-01-01T00:00:00.000Z",
        );
        let initial_jsonl = to_jsonl(std::slice::from_ref(&remote_entry));
        let initial_b64 =
            base64::engine::general_purpose::STANDARD.encode(initial_jsonl.as_bytes());

        // Added to the remote log by another workstation between our GET
        // and PUT -- the retry must not lose this.
        let concurrent_entry = entry(
            "proj-a",
            "topic-concurrent",
            "concurrent fact",
            "2026-02-01T00:00:00.000Z",
        );
        let updated_jsonl = to_jsonl(&[remote_entry, concurrent_entry]);
        let updated_b64 =
            base64::engine::general_purpose::STANDARD.encode(updated_jsonl.as_bytes());

        let server = MockServer::start(vec![
            // ensure_branch: branch already exists.
            MockResponse::json(200, r#"{"object":{"sha":"tip"}}"#),
            // get_file: initial remote content.
            MockResponse::json(
                200,
                &format!(r#"{{"sha":"filesha","content":"{initial_b64}","encoding":"base64"}}"#),
            ),
            // put_file: someone else pushed in between -- conflict.
            MockResponse::json(409, r#"{"message":"conflict"}"#),
            // get_file: refetch after conflict, now includes the concurrent entry.
            MockResponse::json(
                200,
                &format!(r#"{{"sha":"newfilesha","content":"{updated_b64}","encoding":"base64"}}"#),
            ),
            // put_file: retry succeeds.
            MockResponse::json(200, r#"{"content":{"sha":"final"}}"#),
        ]);
        let client = server.client(Some("tok"));

        let report = run(&conn, &client, &config()).unwrap();
        assert!(report.pushed);
        assert_eq!(
            report.imported, 2,
            "the original remote fact and the concurrently-added one should both be imported"
        );

        let all = observations::list_all(&conn).unwrap();
        assert!(all.iter().any(|o| o.title == "remote fact"));
        assert!(all.iter().any(|o| o.title == "local fact"));
        assert!(
            all.iter().any(|o| o.title == "concurrent fact"),
            "the entry added between our GET and PUT must survive the retry"
        );

        let reqs = server.requests();
        assert_eq!(reqs.len(), 5);
        assert_eq!(reqs[2].method, "PUT");
        assert_eq!(reqs[4].method, "PUT");
        let retry_sent: serde_json::Value = serde_json::from_str(&reqs[4].body).unwrap();
        assert_eq!(
            retry_sent["sha"], "newfilesha",
            "retry must use the fresh sha"
        );
        let retry_content = base64::engine::general_purpose::STANDARD
            .decode(retry_sent["content"].as_str().unwrap())
            .unwrap();
        assert!(
            String::from_utf8(retry_content)
                .unwrap()
                .contains("concurrent fact"),
            "the retry payload must carry the concurrently-added entry, not the stale first merge"
        );
    }
}
