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
  bandit_alpha REAL NOT NULL DEFAULT 1.0,
  bandit_beta REAL NOT NULL DEFAULT 1.0,
  shadow_path TEXT,
  PRIMARY KEY (name, source)
);
CREATE VIRTUAL TABLE IF NOT EXISTS skills_fts USING fts5(
  name, description, body, tags, neg_text, content='skills'
);
CREATE TRIGGER IF NOT EXISTS skills_fts_ai AFTER INSERT ON skills BEGIN
  INSERT INTO skills_fts(rowid, name, description, body, tags, neg_text)
  VALUES (new.rowid, new.name, new.description, new.body, new.tags, new.neg_text);
END;
CREATE TRIGGER IF NOT EXISTS skills_fts_ad AFTER DELETE ON skills BEGIN
  INSERT INTO skills_fts(skills_fts, rowid, name, description, body, tags, neg_text)
  VALUES ('delete', old.rowid, old.name, old.description, old.body, old.tags, old.neg_text);
END;
-- Scoped to the indexed columns: `last_used_at` and the bandit counters are
-- updated on every skill load, and a bare AFTER UPDATE would re-tokenize the
-- whole body each time for an index that cannot have changed.
CREATE TRIGGER IF NOT EXISTS skills_fts_au
AFTER UPDATE OF name, description, body, tags, neg_text ON skills BEGIN
  INSERT INTO skills_fts(skills_fts, rowid, name, description, body, tags, neg_text)
  VALUES ('delete', old.rowid, old.name, old.description, old.body, old.tags, old.neg_text);
  INSERT INTO skills_fts(rowid, name, description, body, tags, neg_text)
  VALUES (new.rowid, new.name, new.description, new.body, new.tags, new.neg_text);
END;
CREATE TABLE IF NOT EXISTS skill_impressions (
  name TEXT NOT NULL,
  source TEXT NOT NULL,
  surfaced_at INTEGER NOT NULL,
  PRIMARY KEY (name, source)
);
";

/// Databases written before `skills_fts` became external-content carry the
/// old standalone table, which `CREATE VIRTUAL TABLE IF NOT EXISTS` would
/// leave in place. Drop it so `SCHEMA` recreates the trigger-backed shape,
/// then refill the index from `skills` in the same call -- waiting for the
/// next `rebuild` would leave every `skill_search` empty until then.
///
/// All of it in one transaction, because the halfway state is not
/// self-correcting: with `skills_fts` dropped but not yet refilled, the next
/// open finds no such table, reads `legacy` as 0, and recreates an empty
/// index over a populated `skills` — search silently missing every skill
/// until something forces a full `rebuild`. SQLite makes DDL transactional,
/// so the conversion either lands whole or never happened.
fn apply_schema(conn: &Connection) -> rusqlite::Result<()> {
    let legacy: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master
          WHERE type = 'table' AND name = 'skills_fts' AND sql NOT LIKE '%content=%'",
        [],
        |r| r.get(0),
    )?;
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    if legacy > 0 {
        tx.execute_batch("DROP TABLE skills_fts;")?;
    }
    tx.execute_batch(SCHEMA)?;
    if legacy > 0 {
        tx.execute_batch(
            "INSERT INTO skills_fts(rowid, name, description, body, tags, neg_text)
             SELECT rowid, name, description, body, tags, neg_text FROM skills;",
        )?;
    }
    tx.commit()
}

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
    apply_schema(&conn)?;
    Ok(conn)
}

pub fn open_in_memory() -> rusqlite::Result<Connection> {
    let conn = Connection::open_in_memory()?;
    apply_schema(&conn)?;
    Ok(conn)
}

/// Delete a skill by name and source. Returns true if a row was removed.
pub fn delete_skill(conn: &Connection, name: &str, source: &str) -> rusqlite::Result<bool> {
    // The AFTER DELETE trigger takes the skills_fts row with it.
    let affected = conn.execute(
        "DELETE FROM skills WHERE name = ?1 AND source = ?2",
        params![name, source],
    )?;
    Ok(affected > 0)
}

pub fn rebuild(conn: &mut Connection, entries: &[SkillEntry]) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    // skills_fts follows along through the AFTER DELETE / AFTER INSERT
    // triggers -- including the OR IGNORE case below, where no row is
    // inserted and so nothing is indexed.
    tx.execute("DELETE FROM skills", [])?;
    {
        // OR IGNORE: a single bad skill (duplicate (name, source)) must not
        // roll back the whole rebuild and disable every skill_search/skill_load.
        let mut ins = tx.prepare(
            "INSERT OR IGNORE INTO skills (name, source, path, description, body, neg_text, tags, est_tokens, mtime, last_used_at, bandit_alpha, bandit_beta, shadow_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?11, ?12, ?10)",
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
                e.bandit_alpha,
                e.bandit_beta,
            ])?;
        }
    }
    tx.commit()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Rows the index can actually reach. Deliberately not
    /// `SELECT count(*) FROM skills_fts`: now that the table is
    /// external-content, a bare scan is answered from `skills` itself, so it
    /// reports every skill row whether or not it is indexed. Only a MATCH
    /// goes through the index.
    fn fts_hits(conn: &Connection, term: &str) -> i64 {
        conn.query_row(
            "SELECT count(*) FROM skills_fts WHERE skills_fts MATCH ?1",
            params![term],
            |r| r.get(0),
        )
        .unwrap()
    }

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
            bandit_alpha: 1.0,
            bandit_beta: 1.0,
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
        assert_eq!(fts_hits(&conn, "alpha"), 1);

        rebuild(&mut conn, &[entry("c", "s", "gamma desc")]).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM skills", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(fts_hits(&conn, "gamma"), 1);
        assert_eq!(
            fts_hits(&conn, "alpha"),
            0,
            "a replaced skill must not stay matchable"
        );
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
        assert_eq!(fts_hits(&conn, "first"), 1);
        assert_eq!(
            fts_hits(&conn, "second"),
            0,
            "the ignored duplicate must not be indexed"
        );
        let hits = crate::search::search(&conn, "first", 5, crate::search::MatchMode::All).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "dup");
    }

    #[test]
    fn delete_skill_removes_its_fts_row() {
        // Previously left behind: search() JOINs skills, so a dangling row
        // was invisible rather than wrong -- but it was one more place the
        // manual sync had already been forgotten.
        let mut conn = open_in_memory().unwrap();
        rebuild(&mut conn, &[entry("a", "s", "alpha desc")]).unwrap();
        assert_eq!(fts_hits(&conn, "alpha"), 1);

        assert!(delete_skill(&conn, "a", "s").unwrap());

        assert_eq!(fts_hits(&conn, "alpha"), 0);
    }

    #[test]
    fn a_legacy_standalone_fts_is_converted_and_refilled_on_open() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("skills.db");
        // The pre-conversion shape, with a skill already indexed.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE skills (
                   name TEXT NOT NULL, source TEXT NOT NULL, path TEXT NOT NULL,
                   description TEXT NOT NULL DEFAULT '', body TEXT NOT NULL DEFAULT '',
                   neg_text TEXT NOT NULL DEFAULT '', tags TEXT NOT NULL DEFAULT '',
                   est_tokens INTEGER NOT NULL DEFAULT 0, mtime INTEGER NOT NULL DEFAULT 0,
                   last_used_at INTEGER NOT NULL DEFAULT 0,
                   bandit_alpha REAL NOT NULL DEFAULT 1.0, bandit_beta REAL NOT NULL DEFAULT 1.0,
                   shadow_path TEXT, PRIMARY KEY (name, source));
                 CREATE VIRTUAL TABLE skills_fts USING fts5(name, description, body, tags, neg_text);
                 INSERT INTO skills (name, source, path, description) VALUES ('a', 's', '/p', 'alpha');
                 INSERT INTO skills_fts(rowid, name, description, body, tags, neg_text)
                    VALUES (1, 'a', 'alpha', '', '', '');",
            )
            .unwrap();
        }

        let conn = open_db(&path).unwrap();

        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'skills_fts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(sql.contains("content='skills'"), "must convert: {sql}");
        // Refilled in the same open: waiting for the next rebuild would leave
        // every skill_search empty in between.
        let hits = crate::search::search(&conn, "alpha", 5, crate::search::MatchMode::All).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "a");
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
