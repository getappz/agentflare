//! SQLite persistence: skills table + FTS5 index. Both are derived data;
//! the filesystem is the source of truth, so rebuild is always full-replace
//! inside one transaction.

use crate::sources::SkillEntry;
use rusqlite::{Connection, Transaction, params};
use rusqlite_migration::{HookResult, M, Migrations};
use std::path::Path;

/// Run PRAGMA integrity_check. `Ok(None)` means clean, `Ok(Some(msg))` means
/// SQLite itself reported corruption. `Err` means the check couldn't run at
/// all (locked, permission denied, I/O error, ...) -- distinct from
/// corruption, since the file may be perfectly fine.
pub fn integrity_check(conn: &Connection) -> rusqlite::Result<Option<String>> {
    let result: String = conn.pragma_query_value(None, "integrity_check", |r| r.get(0))?;
    Ok(if result == "ok" { None } else { Some(result) })
}

/// Open DB with repair: if integrity check reports actual corruption, or the
/// file itself won't open (a genuine `rusqlite::Error` -- corruption, not a
/// valid SQLite file, etc.), delete the DB file and create a fresh one.
/// Deliberately does NOT repair-by-delete when the integrity check itself
/// fails to run (locked/permission/I-O error -- not evidence of corruption),
/// on `Migration` (a real bug in a migration -- deleting the file would
/// silently destroy `skill_impressions`/ranking state the filesystem can't
/// reconstruct, hiding the bug instead of surfacing it), or on `SchemaAhead`
/// (means a newer build already wrote to this file; its own error message
/// says not to touch it by hand, let alone delete it).
pub fn open_or_repair(db_path: &Path) -> Result<Connection, db_kit::open::Error> {
    match open_db(db_path) {
        Ok(conn) => match integrity_check(&conn) {
            Ok(None) => Ok(conn),
            Ok(Some(_)) => {
                drop(conn);
                let _ = std::fs::remove_file(db_path);
                open_db(db_path)
            }
            Err(e) => Err(db_kit::open::Error::Sqlite(e)),
        },
        Err(db_kit::open::Error::Sqlite(_)) => {
            let _ = std::fs::remove_file(db_path);
            open_db(db_path)
        }
        Err(e) => Err(e),
    }
}

/// Ranking/FTS columns added to `skills` after `0001_initial` -- (name,
/// declared type + default). Added via a hook rather than baked into
/// `0002_ranking_and_fts.sql` because `ALTER TABLE ADD COLUMN` is not
/// idempotent (unlike the `IF NOT EXISTS` DDL everywhere else in this
/// crate): a `skills.db` first created any time after these columns joined
/// the hand-rolled `apply_schema()` SCHEMA (pre-migration) already has them,
/// and a blind `ALTER TABLE` would error `duplicate column name` on those.
/// Checking `PRAGMA table_info` first makes the migration correct against
/// both that shape and the pre-ranking narrow table `0001_initial` targets.
const RANKING_COLUMNS: &[(&str, &str)] = &[
    ("body", "TEXT NOT NULL DEFAULT ''"),
    ("neg_text", "TEXT NOT NULL DEFAULT ''"),
    ("last_used_at", "INTEGER NOT NULL DEFAULT 0"),
    ("bandit_alpha", "REAL NOT NULL DEFAULT 1.0"),
    ("bandit_beta", "REAL NOT NULL DEFAULT 1.0"),
];

/// Always dropped and recreated from scratch rather than converted in place:
/// this runs once per database (migration hooks run exactly once, tracked by
/// `user_version`), so it must be correct whether `skills_fts`/its triggers
/// never existed, are the pre-external-content standalone shape, or already
/// match this exact shape (e.g. created by a pre-migration `apply_schema()`
/// run against a `skills` table that was still missing these columns --
/// `CREATE TRIGGER`/`CREATE VIRTUAL TABLE` don't validate referenced columns
/// at creation time, only when the trigger fires). `DROP ... IF EXISTS` plus
/// a fresh `CREATE` collapses all of those into one path instead of
/// branching on which one it is. The final `INSERT ... SELECT` refills the
/// index immediately so `skill_search` isn't empty until the next `rebuild`
/// -- on a brand-new database `skills` has no rows yet, so it's a harmless
/// no-op there.
///
/// Frozen history for `0002`, same rule as the `.sql` migration files: never
/// edit this FTS shape after release (new databases would get the new shape
/// at `0002` while existing databases keep the old one, with nothing to
/// reconcile the difference) -- change it in a new `000N_*.sql` migration
/// instead.
const FTS_AND_TRIGGERS: &str = "
DROP TABLE IF EXISTS skills_fts;
DROP TRIGGER IF EXISTS skills_fts_ai;
DROP TRIGGER IF EXISTS skills_fts_ad;
DROP TRIGGER IF EXISTS skills_fts_au;
CREATE VIRTUAL TABLE skills_fts USING fts5(
  name, description, body, tags, neg_text, content='skills'
);
CREATE TRIGGER skills_fts_ai AFTER INSERT ON skills BEGIN
  INSERT INTO skills_fts(rowid, name, description, body, tags, neg_text)
  VALUES (new.rowid, new.name, new.description, new.body, new.tags, new.neg_text);
END;
CREATE TRIGGER skills_fts_ad AFTER DELETE ON skills BEGIN
  INSERT INTO skills_fts(skills_fts, rowid, name, description, body, tags, neg_text)
  VALUES ('delete', old.rowid, old.name, old.description, old.body, old.tags, old.neg_text);
END;
-- Scoped to the indexed columns: `last_used_at` and the bandit counters are
-- updated on every skill load, and a bare AFTER UPDATE would re-tokenize the
-- whole body each time for an index that cannot have changed.
CREATE TRIGGER skills_fts_au
AFTER UPDATE OF name, description, body, tags, neg_text ON skills BEGIN
  INSERT INTO skills_fts(skills_fts, rowid, name, description, body, tags, neg_text)
  VALUES ('delete', old.rowid, old.name, old.description, old.body, old.tags, old.neg_text);
  INSERT INTO skills_fts(rowid, name, description, body, tags, neg_text)
  VALUES (new.rowid, new.name, new.description, new.body, new.tags, new.neg_text);
END;
INSERT INTO skills_fts(rowid, name, description, body, tags, neg_text)
SELECT rowid, name, description, body, tags, neg_text FROM skills;
";

fn add_ranking_columns_and_fts(tx: &Transaction) -> HookResult {
    let existing: std::collections::HashSet<String> = tx
        .prepare("PRAGMA table_info(skills)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<Result<_, _>>()?;
    for (col, decl) in RANKING_COLUMNS {
        if !existing.contains(*col) {
            tx.execute(&format!("ALTER TABLE skills ADD COLUMN {col} {decl}"), [])?;
        }
    }
    tx.execute_batch(FTS_AND_TRIGGERS)?;
    Ok(())
}

/// Schema history, oldest first -- mirrors the crate's real history:
/// `0001_initial` is the original (#92) narrow `skills` table + standalone
/// FTS5 index; `0002_ranking_and_fts` is the bandit-ranking epic (#302) that
/// added `skill_impressions` plus the ranking columns and external-content
/// FTS5 index with sync triggers (#347). `IF NOT EXISTS` throughout 0001
/// means replaying it against a pre-migration db (`user_version` still 0) is
/// a harmless no-op; the hook in 0002 is what makes replaying safe for a db
/// that already has some or all of the ranking columns. Future schema
/// changes are new `000N_*.sql` files appended here, never edits to these.
///
/// `LazyLock`, not `const`, because `M::up_with_hook` boxes its hook and so
/// isn't a `const fn` (unlike the plain-SQL `M::up` other migrations use).
static MIGRATIONS: std::sync::LazyLock<Migrations<'static>> = std::sync::LazyLock::new(|| {
    Migrations::new(vec![
        M::up(include_str!("migrations/0001_initial.sql")),
        M::up_with_hook(
            include_str!("migrations/0002_ranking_and_fts.sql"),
            add_ranking_columns_and_fts,
        ),
    ])
});

pub fn open_db(path: &Path) -> Result<Connection, db_kit::open::Error> {
    // `db_kit::open_file` already sets a 5s busy_timeout and WAL journal
    // mode -- shared across all agentflare MCP processes, so readers and a
    // writer can proceed concurrently instead of hitting SQLITE_BUSY.
    db_kit::open_file(path, &MIGRATIONS)
}

pub fn open_in_memory() -> Result<Connection, db_kit::open::Error> {
    db_kit::open_memory(&MIGRATIONS)
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
    fn a_pre_ranking_narrow_skills_table_gains_columns_without_erroring() {
        // Reproduces the real-world bug: a `skills.db` created before the
        // ranking epic (#302) has the original 8-column `skills` table, but
        // a later `apply_schema()` run already created the modern FTS5
        // triggers referencing `body`/`neg_text` on top of it (`CREATE
        // VIRTUAL TABLE`/`CREATE TRIGGER ... IF NOT EXISTS` never validate
        // the referenced columns at creation time). The first DELETE or
        // qualifying UPDATE against `skills` then fails with `no such
        // column: old.body`.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("skills.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE skills (
                   name TEXT NOT NULL, source TEXT NOT NULL, path TEXT NOT NULL,
                   description TEXT NOT NULL DEFAULT '', tags TEXT NOT NULL DEFAULT '',
                   est_tokens INTEGER NOT NULL DEFAULT 0, mtime INTEGER NOT NULL DEFAULT 0,
                   shadow_path TEXT, PRIMARY KEY (name, source));
                 INSERT INTO skills (name, source, path, description) VALUES ('a', 's', '/p', 'alpha');
                 CREATE VIRTUAL TABLE skills_fts USING fts5(
                   name, description, body, tags, neg_text, content='skills'
                 );
                 CREATE TRIGGER skills_fts_ad AFTER DELETE ON skills BEGIN
                   INSERT INTO skills_fts(skills_fts, rowid, name, description, body, tags, neg_text)
                   VALUES ('delete', old.rowid, old.name, old.description, old.body, old.tags, old.neg_text);
                 END;",
            )
            .unwrap();
        }

        let mut conn = open_db(&path).unwrap();

        let cols: std::collections::HashSet<String> = conn
            .prepare("PRAGMA table_info(skills)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for col in [
            "body",
            "neg_text",
            "last_used_at",
            "bandit_alpha",
            "bandit_beta",
        ] {
            assert!(cols.contains(col), "missing column: {col}");
        }

        // The previously-broken path: rebuild()'s DELETE FROM skills used to
        // fail firing the AFTER DELETE trigger with "no such column: old.body".
        rebuild(&mut conn, &[entry("a", "s", "alpha desc")]).unwrap();
        assert_eq!(fts_hits(&conn, "alpha"), 1);
    }

    #[test]
    fn open_or_repair_propagates_schema_ahead_instead_of_deleting_the_db() {
        // A newer build already migrated this file past what MIGRATIONS here
        // knows about. Deleting it would silently destroy real data
        // (skill_impressions/ranking state the filesystem can't
        // reconstruct) instead of surfacing the actual problem.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("skills.db");
        {
            let mut conn = open_db(&path).unwrap();
            rebuild(&mut conn, &[entry("a", "s", "alpha desc")]).unwrap();
            conn.pragma_update(None, "user_version", 999).unwrap();
        }

        let err = open_or_repair(&path).unwrap_err();
        assert!(
            matches!(err, db_kit::open::Error::SchemaAhead { .. }),
            "expected SchemaAhead, got {err:?}"
        );
        assert!(path.exists(), "SchemaAhead must not delete the db file");

        // The data survives: a plain rusqlite open (bypassing migrations)
        // still finds the row that was there before open_or_repair ran.
        let conn = Connection::open(&path).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM skills", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "open_or_repair must not have wiped the db");
    }

    #[test]
    fn open_or_repair_deletes_and_recreates_on_a_corrupt_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("skills.db");
        std::fs::write(&path, b"not a sqlite file").unwrap();

        let conn = open_or_repair(&path).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM skills", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "corrupt file must be replaced with a fresh db");
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
