//! Read-only memory quality diagnostics for `agentflare doctor`.
//! Never opens the brain DB for write and never mutates rows.

use super::observations::normalize_text;
use super::store;
use rusqlite::{Connection, OpenFlags, params};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Observations with no update / last-seen within this many days count as stale.
pub const DEFAULT_STALE_DAYS: i64 = 30;

/// Liveness (DB reachable) vs readiness (embedding model) plus quality counts.
/// Quality findings are diagnostic only — they never write.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MemoryQualityReport {
    /// Brain DB file exists and opened read-only successfully.
    pub db_reachable: bool,
    /// Embedding engine loaded (semantic feature + model available).
    pub embedding_ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding_unavailable_reason: Option<String>,
    /// Live observations that share a `normalized_hash` or near-identical title
    /// with at least one other live observation.
    pub duplicate_candidates: u64,
    /// Live observations whose latest activity (`last_seen_at` else `updated_at`)
    /// is older than `stale_after_days`.
    pub stale_observations: u64,
    /// Live observations with NULL or empty `project`.
    pub no_project: u64,
    /// Live observations with no row in `observations_vec`.
    pub missing_embeddings: u64,
    pub total_observations: u64,
    pub stale_after_days: i64,
}

impl MemoryQualityReport {
    pub fn is_clean(&self) -> bool {
        self.duplicate_candidates == 0
            && self.stale_observations == 0
            && self.no_project == 0
            && self.missing_embeddings == 0
    }

    /// Probe the on-disk brain DB read-only. Missing file → unreachable + clean.
    pub fn collect() -> Self {
        collect_at(&store::brain_db_path(), DEFAULT_STALE_DAYS)
    }

    pub fn format_text(&self) -> String {
        let db = if self.db_reachable {
            "reachable"
        } else {
            "unreachable"
        };
        let emb = match &self.embedding_unavailable_reason {
            None => "ready".to_string(),
            Some(why) => format!("not ready ({why})"),
        };
        let quality = if !self.db_reachable {
            "skipped (db unreachable)".to_string()
        } else if self.is_clean() {
            if self.total_observations == 0 {
                "clean (empty)".to_string()
            } else {
                "clean".to_string()
            }
        } else {
            format!(
                "{} duplicate candidates, {} stale, {} no project, {} missing embeddings (of {} observations)",
                self.duplicate_candidates,
                self.stale_observations,
                self.no_project,
                self.missing_embeddings,
                self.total_observations
            )
        };
        format!("  memory: db {db}, embedding {emb}\n    quality: {quality}")
    }
}

fn collect_at(path: &Path, stale_after_days: i64) -> MemoryQualityReport {
    let (embedding_ready, embedding_unavailable_reason) = match super::engine::unavailable_reason()
    {
        None => (true, None),
        Some(why) => (false, Some(why)),
    };
    if !path.exists() {
        return MemoryQualityReport {
            db_reachable: false,
            embedding_ready,
            embedding_unavailable_reason,
            duplicate_candidates: 0,
            stale_observations: 0,
            no_project: 0,
            missing_embeddings: 0,
            total_observations: 0,
            stale_after_days,
        };
    }
    match open_readonly(path).and_then(|conn| analyze(&conn, stale_after_days)) {
        Ok(mut report) => {
            report.db_reachable = true;
            report.embedding_ready = embedding_ready;
            report.embedding_unavailable_reason = embedding_unavailable_reason;
            report
        }
        Err(_) => MemoryQualityReport {
            db_reachable: false,
            embedding_ready,
            embedding_unavailable_reason,
            duplicate_candidates: 0,
            stale_observations: 0,
            no_project: 0,
            missing_embeddings: 0,
            total_observations: 0,
            stale_after_days,
        },
    }
}

fn open_readonly(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
}

/// Score an already-open connection (tests seed in-memory DBs here).
pub fn analyze(conn: &Connection, stale_after_days: i64) -> rusqlite::Result<MemoryQualityReport> {
    let total_observations: u64 = conn.query_row(
        "SELECT COUNT(*) FROM observations WHERE deleted_at IS NULL",
        [],
        |r| r.get::<_, i64>(0).map(|n| n as u64),
    )?;
    let no_project: u64 = conn.query_row(
        "SELECT COUNT(*) FROM observations
         WHERE deleted_at IS NULL AND (project IS NULL OR project = '')",
        [],
        |r| r.get::<_, i64>(0).map(|n| n as u64),
    )?;
    let missing_embeddings: u64 = conn.query_row(
        "SELECT COUNT(*) FROM observations o
         LEFT JOIN observations_vec v ON v.obs_id = o.id
         WHERE o.deleted_at IS NULL AND v.obs_id IS NULL",
        [],
        |r| r.get::<_, i64>(0).map(|n| n as u64),
    )?;
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(stale_after_days)).to_rfc3339();
    let stale_observations: u64 = conn.query_row(
        "SELECT COUNT(*) FROM observations
         WHERE deleted_at IS NULL
           AND COALESCE(last_seen_at, updated_at) < ?1",
        params![cutoff],
        |r| r.get::<_, i64>(0).map(|n| n as u64),
    )?;
    let duplicate_candidates = count_duplicate_candidates(conn)?;

    Ok(MemoryQualityReport {
        db_reachable: true,
        embedding_ready: false,
        embedding_unavailable_reason: None,
        duplicate_candidates,
        stale_observations,
        no_project,
        missing_embeddings,
        total_observations,
        stale_after_days,
    })
}

fn count_duplicate_candidates(conn: &Connection) -> rusqlite::Result<u64> {
    let mut stmt = conn
        .prepare("SELECT id, title, normalized_hash FROM observations WHERE deleted_at IS NULL")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;

    let mut by_hash: HashMap<String, Vec<i64>> = HashMap::new();
    let mut by_title: HashMap<String, Vec<i64>> = HashMap::new();
    for row in rows {
        let (id, title, hash) = row?;
        if let Some(h) = hash.filter(|h| !h.is_empty()) {
            by_hash.entry(h).or_default().push(id);
        }
        let norm = near_identical_title_key(&title);
        if !norm.is_empty() {
            by_title.entry(norm).or_default().push(id);
        }
    }

    let mut candidates = HashSet::new();
    for ids in by_hash.values().chain(by_title.values()) {
        if ids.len() >= 2 {
            candidates.extend(ids.iter().copied());
        }
    }
    Ok(candidates.len() as u64)
}

/// Near-identical title key: whitespace-collapsed lowercase, `_` → space
/// (potpie identity normalize + our content normalize).
fn near_identical_title_key(title: &str) -> String {
    normalize_text(&title.replace('_', " "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{embeddings, observations, schema};
    use crate::paths::test_support::with_temp_home;
    use rusqlite::Connection;

    fn new_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        schema::migrate(&mut conn).unwrap();
        conn
    }

    fn insert_raw(
        conn: &Connection,
        title: &str,
        content: &str,
        project: Option<&str>,
        hash: &str,
        updated_at: &str,
        last_seen_at: Option<&str>,
    ) -> i64 {
        conn.execute(
            "INSERT INTO observations
                (type, title, content, project, scope, normalized_hash,
                 created_at, updated_at, last_seen_at)
             VALUES ('note', ?1, ?2, ?3, 'project', ?4, ?5, ?5, ?6)",
            params![title, content, project, hash, updated_at, last_seen_at],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn empty_memory_reports_clean() {
        let conn = new_db();
        let report = analyze(&conn, DEFAULT_STALE_DAYS).unwrap();
        assert!(report.is_clean());
        assert_eq!(report.total_observations, 0);
        assert_eq!(report.duplicate_candidates, 0);
        assert_eq!(report.stale_observations, 0);
        assert_eq!(report.no_project, 0);
        assert_eq!(report.missing_embeddings, 0);
    }

    #[test]
    fn seeded_counts_match() {
        let conn = new_db();
        let old = "2000-01-01T00:00:00.000Z";
        let recent = chrono::Utc::now().to_rfc3339();

        // Two rows sharing a hash → 2 duplicate candidates.
        insert_raw(
            &conn,
            "Alpha",
            "body-a",
            Some("p"),
            "hash-dup",
            &recent,
            None,
        );
        insert_raw(
            &conn,
            "Alpha2",
            "body-b",
            Some("p"),
            "hash-dup",
            &recent,
            None,
        );

        // Near-identical titles (underscore vs space), distinct hashes → +2.
        insert_raw(
            &conn,
            "Payments API",
            "one",
            Some("p"),
            "h-title-1",
            &recent,
            None,
        );
        insert_raw(
            &conn,
            "payments_api",
            "two",
            Some("p"),
            "h-title-2",
            &recent,
            None,
        );

        // Stale (old updated_at, no last_seen) + no project + missing embedding.
        insert_raw(&conn, "orphan stale", "x", None, "h-stale", old, None);

        // Fresh with project but still missing embedding.
        insert_raw(
            &conn,
            "fresh covered later",
            "y",
            Some("p"),
            "h-fresh",
            &recent,
            Some(&recent),
        );
        let covered = conn.last_insert_rowid();
        embeddings::upsert(&conn, covered, &[1.0, 0.0], "test").unwrap();

        let report = analyze(&conn, DEFAULT_STALE_DAYS).unwrap();
        assert_eq!(report.total_observations, 6);
        assert_eq!(report.duplicate_candidates, 4);
        assert_eq!(report.stale_observations, 1);
        assert_eq!(report.no_project, 1);
        // 6 live − 1 with embedding = 5 missing
        assert_eq!(report.missing_embeddings, 5);
        assert!(!report.is_clean());
    }

    #[test]
    fn analyze_does_not_mutate() {
        let conn = new_db();
        let id =
            match observations::save(&conn, None, "note", "t", "c", None, Some("p"), None, None)
                .unwrap()
            {
                observations::SaveOutcome::Created(id) => id,
                other => panic!("expected Created, got {other:?}"),
            };
        let before: String = conn
            .query_row(
                "SELECT updated_at FROM observations WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        let count_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))
            .unwrap();

        let _ = analyze(&conn, DEFAULT_STALE_DAYS).unwrap();

        let after: String = conn
            .query_row(
                "SELECT updated_at FROM observations WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        let count_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, after);
        assert_eq!(count_before, count_after);
    }

    #[test]
    fn missing_brain_file_is_unreachable_and_clean() {
        with_temp_home(|| {
            let report = MemoryQualityReport::collect();
            assert!(!report.db_reachable);
            assert!(report.is_clean());
            assert_eq!(report.total_observations, 0);
        });
    }

    #[test]
    fn open_readonly_rejects_writes() {
        with_temp_home(|| {
            let path = store::brain_db_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            // Create via the normal write path, then reopen read-only.
            drop(store::open().unwrap());
            let ro = open_readonly(&path).unwrap();
            let err = ro.execute(
                "INSERT INTO observations (type, title, content, created_at, updated_at)
                 VALUES ('note', 'x', 'y', 't', 't')",
                [],
            );
            assert!(err.is_err(), "read-only open must reject writes");
        });
    }

    #[test]
    fn soft_deleted_rows_are_ignored() {
        let conn = new_db();
        let recent = chrono::Utc::now().to_rfc3339();
        let a = insert_raw(&conn, "Same", "a", Some("p"), "h", &recent, None);
        insert_raw(&conn, "Same", "b", Some("p"), "h", &recent, None);
        observations::soft_delete(&conn, a).unwrap();
        let report = analyze(&conn, DEFAULT_STALE_DAYS).unwrap();
        assert_eq!(report.duplicate_candidates, 0);
        assert_eq!(report.total_observations, 1);
    }
}
