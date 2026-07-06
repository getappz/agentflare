use rusqlite::Connection;
use std::path::{Path, PathBuf};

const SCHEMA_V1: &str = "
CREATE TABLE session_files (
    file_path   TEXT PRIMARY KEY,
    mtime_secs  INTEGER NOT NULL,
    size_bytes  INTEGER NOT NULL,
    indexed_at  TEXT NOT NULL
);

CREATE TABLE file_rollup (
    file_path                 TEXT NOT NULL,
    date                      TEXT NOT NULL,
    project                   TEXT NOT NULL,
    model                     TEXT NOT NULL,
    input_tokens              INTEGER NOT NULL,
    output_tokens             INTEGER NOT NULL,
    cache_read_tokens         INTEGER NOT NULL,
    cache_creation_tokens     INTEGER NOT NULL,
    cache_creation_5m_tokens  INTEGER NOT NULL,
    cache_creation_1hr_tokens INTEGER NOT NULL,
    cost_usd                  REAL NOT NULL,
    has_unpriced_usage        INTEGER NOT NULL,
    PRIMARY KEY (file_path, date, model)
);
CREATE INDEX file_rollup_date ON file_rollup(date);
";

fn db_path() -> PathBuf {
    crate::state::state_dir().join("analytics.db")
}

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 1 {
        conn.execute_batch(SCHEMA_V1)?;
        conn.pragma_update(None, "user_version", 1)?;
    }
    Ok(())
}

fn try_open(path: &Path) -> Option<Connection> {
    let conn = Connection::open(path).ok()?;
    migrate(&conn).ok()?;
    Some(conn)
}

/// Opens the analytics cache database, creating and migrating it if needed.
/// The database is a pure cache over the JSONL session transcripts, so any
/// corruption is recovered by deleting and recreating it — never by partial
/// repair. If the filesystem is entirely unusable, falls back to an
/// in-memory database so `agentflare cost` still works for this run (without
/// persisting the cache).
pub(crate) fn open_or_rebuild() -> Connection {
    let path = db_path();
    let _ = std::fs::create_dir_all(crate::state::state_dir());

    if let Some(conn) = try_open(&path) {
        return conn;
    }
    let _ = std::fs::remove_file(&path);
    if let Some(conn) = try_open(&path) {
        return conn;
    }
    Connection::open_in_memory().expect("sqlite failed to open even an in-memory database")
}

use crate::cost::{
    add_tokens, find_session_files_under, project_name_for, should_count_line, GroupTotals,
};
use chrono::NaiveDate;
use rusqlite::params;
use std::collections::{HashMap, HashSet};

/// Walks `projects_dir` for session files, skips any whose (mtime, size)
/// still matches its `session_files` catalog entry, and fully reparses +
/// re-persists any new or changed file. See the design doc for why whole-file
/// reparse-on-change (not byte-offset incremental) is required to keep the
/// message.id:requestId dedup guarantee exact.
pub(crate) fn sync(conn: &mut Connection, projects_dir: &Path) {
    let pricing = crate::pricing::load_pricing();
    for path in find_session_files_under(projects_dir) {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        let Ok(mtime) = meta.modified() else {
            continue;
        };
        let mtime_secs = mtime
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let size_bytes = meta.len() as i64;
        let path_str = path.to_string_lossy().to_string();

        let catalog_match: Option<(i64, i64)> = conn
            .query_row(
                "SELECT mtime_secs, size_bytes FROM session_files WHERE file_path = ?1",
                params![path_str],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();

        if catalog_match == Some((mtime_secs, size_bytes)) {
            continue;
        }

        reindex_file(conn, &path, &path_str, mtime_secs, size_bytes, &pricing);
    }
}

fn reindex_file(
    conn: &mut Connection,
    path: &Path,
    path_str: &str,
    mtime_secs: i64,
    size_bytes: i64,
    pricing: &HashMap<String, crate::pricing::ModelPricing>,
) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    let project = project_name_for(path);
    let mut seen: HashSet<String> = HashSet::new();
    let mut sums: HashMap<(NaiveDate, String), GroupTotals> = HashMap::new();

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Some(parsed) = crate::cost::parse_line(line) else {
            continue;
        };
        let Some(date) = parsed.date else {
            continue;
        };
        if !should_count_line(&parsed, &mut seen) {
            continue;
        }

        let model = parsed.model.clone().unwrap_or_else(|| "unknown".to_string());
        let cost = crate::pricing::calculate_cost(&parsed.tokens, parsed.model.as_deref(), pricing);

        let entry = sums.entry((date, model)).or_default();
        add_tokens(&mut entry.tokens, &parsed.tokens);
        entry.cost_usd += cost.total_usd;
        entry.has_unpriced_usage |= cost.has_unpriced_usage;
    }

    let tx = conn.transaction().expect("failed to open sqlite transaction");
    tx.execute("DELETE FROM file_rollup WHERE file_path = ?1", params![path_str])
        .expect("failed to clear stale file_rollup rows");

    for ((date, model), totals) in &sums {
        tx.execute(
            "INSERT INTO file_rollup (
                file_path, date, project, model,
                input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
                cache_creation_5m_tokens, cache_creation_1hr_tokens,
                cost_usd, has_unpriced_usage
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                path_str,
                date.to_string(),
                project,
                model,
                totals.tokens.input_tokens as i64,
                totals.tokens.output_tokens as i64,
                totals.tokens.cache_read_tokens as i64,
                totals.tokens.cache_creation_tokens as i64,
                totals.tokens.cache_creation_5m_tokens as i64,
                totals.tokens.cache_creation_1hr_tokens as i64,
                totals.cost_usd,
                totals.has_unpriced_usage as i64,
            ],
        )
        .expect("failed to insert file_rollup row");
    }

    tx.execute(
        "INSERT INTO session_files (file_path, mtime_secs, size_bytes, indexed_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(file_path) DO UPDATE SET
            mtime_secs = excluded.mtime_secs,
            size_bytes = excluded.size_bytes,
            indexed_at = excluded.indexed_at",
        params![path_str, mtime_secs, size_bytes, chrono::Local::now().to_rfc3339()],
    )
    .expect("failed to upsert session_files catalog row");

    tx.commit().expect("failed to commit rollup transaction");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;

    #[test]
    fn open_or_rebuild_creates_expected_schema() {
        with_temp_home(|| {
            let conn = open_or_rebuild();
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
                .unwrap();
            let names: Vec<String> = stmt
                .query_map([], |row| row.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            assert_eq!(names, vec!["file_rollup", "session_files"]);

            let version: i32 = conn
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            assert_eq!(version, 1);
        });
    }

    #[test]
    fn open_or_rebuild_is_idempotent() {
        with_temp_home(|| {
            let _ = open_or_rebuild();
            let conn = open_or_rebuild();
            let version: i32 = conn
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            assert_eq!(version, 1);
        });
    }

    #[test]
    fn open_or_rebuild_recovers_from_corrupt_db_file() {
        with_temp_home(|| {
            let path = db_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"not a sqlite database").unwrap();

            let conn = open_or_rebuild();
            let version: i32 = conn
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            assert_eq!(version, 1);
        });
    }

    fn write_session_file(dir: &std::path::Path, project: &str, file: &str, content: &str) -> PathBuf {
        let project_dir = dir.join(project);
        std::fs::create_dir_all(&project_dir).unwrap();
        let path = project_dir.join(file);
        std::fs::write(&path, content).unwrap();
        path
    }

    fn row_count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn sync_indexes_a_fresh_file() {
        with_temp_home(|| {
            let dir = std::env::temp_dir().join("agentflare-test-rollup-sync-fresh");
            let _ = std::fs::remove_dir_all(&dir);
            let line = r#"{"type":"assistant","timestamp":"2026-07-06T10:00:00Z","message":{"id":"m1","model":"claude-opus-4-8","usage":{"input_tokens":100,"output_tokens":50}},"requestId":"r1"}"#;
            write_session_file(&dir, "proj1", "session1.jsonl", &format!("{line}\n"));

            let mut conn = open_or_rebuild();
            sync(&mut conn, &dir);

            assert_eq!(row_count(&conn, "session_files"), 1);
            assert_eq!(row_count(&conn, "file_rollup"), 1);
            let (model, input_tokens): (String, i64) = conn
                .query_row(
                    "SELECT model, input_tokens FROM file_rollup",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(model, "claude-opus-4-8");
            assert_eq!(input_tokens, 100);

            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn sync_skips_files_whose_catalog_fingerprint_still_matches() {
        with_temp_home(|| {
            let dir = std::env::temp_dir().join("agentflare-test-rollup-sync-skip");
            let _ = std::fs::remove_dir_all(&dir);
            let line = r#"{"type":"assistant","timestamp":"2026-07-06T10:00:00Z","message":{"id":"m1","model":"claude-opus-4-8","usage":{"input_tokens":100,"output_tokens":50}},"requestId":"r1"}"#;
            write_session_file(&dir, "proj1", "session1.jsonl", &format!("{line}\n"));

            let mut conn = open_or_rebuild();
            sync(&mut conn, &dir);

            // Plant a sentinel value into the already-indexed row. If a
            // second sync() call re-reindexes this unchanged file, the
            // sentinel gets overwritten.
            conn.execute("UPDATE file_rollup SET cost_usd = 999999.0", [])
                .unwrap();

            sync(&mut conn, &dir);

            let cost: f64 = conn
                .query_row("SELECT cost_usd FROM file_rollup", [], |row| row.get(0))
                .unwrap();
            assert_eq!(cost, 999999.0, "unchanged file must not be reprocessed");

            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn sync_reindexes_a_file_after_it_changes() {
        with_temp_home(|| {
            let dir = std::env::temp_dir().join("agentflare-test-rollup-sync-changed");
            let _ = std::fs::remove_dir_all(&dir);
            let line1 = r#"{"type":"assistant","timestamp":"2026-07-06T10:00:00Z","message":{"id":"m1","model":"claude-opus-4-8","usage":{"input_tokens":100,"output_tokens":50}},"requestId":"r1"}"#;
            let path = write_session_file(&dir, "proj1", "session1.jsonl", &format!("{line1}\n"));

            let mut conn = open_or_rebuild();
            sync(&mut conn, &dir);

            let line2 = r#"{"type":"assistant","timestamp":"2026-07-06T11:00:00Z","message":{"id":"m2","model":"claude-opus-4-8","usage":{"input_tokens":20,"output_tokens":10}},"requestId":"r2"}"#;
            std::fs::write(&path, format!("{line1}\n{line2}\n")).unwrap();

            sync(&mut conn, &dir);

            assert_eq!(row_count(&conn, "file_rollup"), 1, "same date+model still one row");
            let input_tokens: i64 = conn
                .query_row("SELECT input_tokens FROM file_rollup", [], |row| row.get(0))
                .unwrap();
            assert_eq!(input_tokens, 120, "reindex must reflect the appended line, not double-count the original");

            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn sync_splits_a_midnight_spanning_file_into_two_date_rows() {
        with_temp_home(|| {
            let dir = std::env::temp_dir().join("agentflare-test-rollup-sync-midnight");
            let _ = std::fs::remove_dir_all(&dir);
            let day1 = r#"{"type":"assistant","timestamp":"2026-07-05T23:00:00Z","message":{"id":"m1","model":"claude-opus-4-8","usage":{"input_tokens":100,"output_tokens":0}},"requestId":"r1"}"#;
            let day2 = r#"{"type":"assistant","timestamp":"2026-07-06T01:00:00Z","message":{"id":"m2","model":"claude-opus-4-8","usage":{"input_tokens":30,"output_tokens":0}},"requestId":"r2"}"#;
            write_session_file(&dir, "proj1", "session1.jsonl", &format!("{day1}\n{day2}\n"));

            let mut conn = open_or_rebuild();
            sync(&mut conn, &dir);

            assert_eq!(row_count(&conn, "file_rollup"), 2);

            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn sync_preserves_message_id_request_id_dedup() {
        with_temp_home(|| {
            let dir = std::env::temp_dir().join("agentflare-test-rollup-sync-dedup");
            let _ = std::fs::remove_dir_all(&dir);
            // Two content-block lines for the same API response (same
            // message.id:requestId) must be counted once, not twice.
            let block1 = r#"{"type":"assistant","timestamp":"2026-07-06T10:00:00Z","message":{"id":"m1","model":"claude-opus-4-8","usage":{"input_tokens":100,"output_tokens":50}},"requestId":"r1"}"#;
            let block2 = r#"{"type":"assistant","timestamp":"2026-07-06T10:00:00Z","message":{"id":"m1","model":"claude-opus-4-8","usage":{"input_tokens":100,"output_tokens":50}},"requestId":"r1"}"#;
            write_session_file(&dir, "proj1", "session1.jsonl", &format!("{block1}\n{block2}\n"));

            let mut conn = open_or_rebuild();
            sync(&mut conn, &dir);

            let input_tokens: i64 = conn
                .query_row("SELECT input_tokens FROM file_rollup", [], |row| row.get(0))
                .unwrap();
            assert_eq!(input_tokens, 100, "duplicate content-block lines must be deduped");

            let _ = std::fs::remove_dir_all(&dir);
        });
    }
}
