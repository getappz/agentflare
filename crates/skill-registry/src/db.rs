//! SQLite persistence: skills table + FTS5 index. Both are derived data;
//! the filesystem is the source of truth, so rebuild is always full-replace
//! inside one transaction.

use crate::sources::SkillEntry;
use rusqlite::{Connection, params};
use std::path::Path;

/// Run PRAGMA integrity_check. Returns error message on failure, None if OK.
pub fn integrity_check(conn: &Connection) -> Option<String> {
    let result: Result<String, _> =
        conn.pragma_query_value(None, "integrity_check", |r| r.get::<_, String>(0));
    match result {
        Ok(s) if s == "ok" => None,
        Ok(s) => Some(s),
        Err(e) => Some(format!("integrity_check query failed: {e}")),
    }
}

/// Open DB with repair: if integrity check fails or open fails, delete the DB
/// file and create a fresh one.
pub fn open_or_repair(db_path: &Path) -> rusqlite::Result<Connection> {
    match open_db(db_path) {
        Ok(conn) => {
            if integrity_check(&conn).is_some() {
                drop(conn);
                let _ = std::fs::remove_file(db_path);
                open_db(db_path)
            } else {
                Ok(conn)
            }
        }
        Err(_) => {
            let _ = std::fs::remove_file(db_path);
            open_db(db_path)
        }
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS skills (
  name TEXT NOT NULL,
  source TEXT NOT NULL,
  path TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  body TEXT NOT NULL DEFAULT '',
  neg_text TEXT NOT NULL DEFAULT '',
  tags TEXT NOT NULL DEFAULT '',
  est_tokens INTEGER NOT NULL DEFAULT 0,
  mtime INTEGER NOT NULL DEFAULT 0,
  last_used_at INTEGER NOT NULL DEFAULT 0,
  shadow_path TEXT,
  PRIMARY KEY (name, source)
);
CREATE VIRTUAL TABLE IF NOT EXISTS skills_fts USING fts5(name, description, body, tags, neg_text);
";

pub fn open_db(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)?;
    // Shared across all agentflare MCP processes: without a busy timeout,
    // concurrent writers hit SQLITE_BUSY immediately instead of waiting;
    // WAL lets readers and a writer proceed concurrently.
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

pub fn open_in_memory() -> rusqlite::Result<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

/// Delete a skill by name and source. Returns true if a row was removed.
pub fn delete_skill(conn: &Connection, name: &str, source: &str) -> rusqlite::Result<bool> {
    let affected = conn.execute(
        "DELETE FROM skills WHERE name = ?1 AND source = ?2",
        params![name, source],
    )?;
    // FTS rowids match skills rowids; a dangling FTS row is harmless
    // because search() JOINs against skills and won't return it.
    Ok(affected > 0)
}

pub fn rebuild(conn: &mut Connection, entries: &[SkillEntry]) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM skills", [])?;
    tx.execute("DELETE FROM skills_fts", [])?;
    {
        // OR IGNORE: a single bad skill (duplicate (name, source)) must not
        // roll back the whole rebuild and disable every skill_search/skill_load.
        let mut ins = tx.prepare(
            "INSERT OR IGNORE INTO skills (name, source, path, description, body, neg_text, tags, est_tokens, mtime, last_used_at, shadow_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10)",
        )?;
        let mut fts = tx.prepare(
            "INSERT INTO skills_fts (rowid, name, description, body, tags, neg_text) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for e in entries {
            ins.execute(params![
                e.name,
                e.source,
                e.path.to_string_lossy(),
                e.description,
                e.body,
                e.neg_text,
                e.tags,
                e.est_tokens,
                e.mtime,
                e.shadow_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string()),
            ])?;
            if tx.changes() == 0 {
                // Duplicate (name, source) was ignored: no new skills row,
                // so skip the fts mirror (last_insert_rowid() would be stale).
                continue;
            }
            let rowid = tx.last_insert_rowid();
            fts.execute(params![
                rowid,
                e.name,
                e.description,
                e.body,
                e.tags,
                e.neg_text
            ])?;
        }
    }
    tx.commit()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(name: &str, source: &str, desc: &str) -> SkillEntry {
        SkillEntry {
            name: name.into(),
            source: source.into(),
            path: PathBuf::from(format!("/x/{name}/SKILL.md")),
            description: desc.into(),
            body: String::new(),
            neg_text: String::new(),
            tags: String::new(),
            est_tokens: 100,
            mtime: 1,
            shadow_path: None,
        }
    }

    #[test]
    fn rebuild_replaces_rows_and_fts_stays_in_sync() {
        let mut conn = open_in_memory().unwrap();
        rebuild(
            &mut conn,
            &[entry("a", "s", "alpha desc"), entry("b", "s", "beta desc")],
        )
        .unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM skills", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
        rebuild(&mut conn, &[entry("c", "s", "gamma desc")]).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM skills", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let f: i64 = conn
            .query_row("SELECT count(*) FROM skills_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(f, 1);
    }

    #[test]
    fn fts_rowids_match_skills_rowids() {
        let mut conn = open_in_memory().unwrap();
        rebuild(&mut conn, &[entry("a", "s", "alpha")]).unwrap();
        let pair: (i64, i64) = conn
            .query_row(
                "SELECT s.rowid, f.rowid FROM skills s, skills_fts f WHERE f.name = s.name",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(pair.0, pair.1);
    }

    #[test]
    fn rebuild_tolerates_duplicate_name_source_without_failing_whole_batch() {
        let mut conn = open_in_memory().unwrap();
        rebuild(
            &mut conn,
            &[
                entry("dup", "s", "first desc"),
                entry("dup", "s", "second desc"),
            ],
        )
        .unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM skills", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let f: i64 = conn
            .query_row("SELECT count(*) FROM skills_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(f, 1);
        let hits = crate::search::search(&conn, "first", 5, crate::search::MatchMode::All).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "dup");
    }

    #[test]
    fn open_db_sets_wal_journal_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("skills.db")).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }
}
