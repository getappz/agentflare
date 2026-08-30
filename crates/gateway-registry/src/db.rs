//! SQLite persistence: tools table + FTS5 index. Both are derived data —
//! live `tools/list` responses from connected downstream servers are the
//! source of truth, so rebuild is always full-replace inside one
//! transaction (same pattern as `crates/skill-registry/src/db.rs`).

use crate::types::ToolEntry;
use rusqlite::{Connection, params};
use rusqlite_migration::{M, Migrations};
use std::path::Path;

/// Schema history, oldest first -- mirrors the crate's real history:
/// `0001_initial` is the original (#104) `tools` table + standalone FTS5
/// index; `0002_fts_triggers` (#347) converts it to the external-content
/// shape with sync triggers. Unlike `crates/skill-registry/src/db.rs`
/// (which this crate otherwise mirrors), `tools`'s columns have never
/// changed since #104, so no `ALTER TABLE`/migration hook is needed here --
/// `0002` unconditionally drops and recreates `tools_fts` (`DROP ... IF
/// EXISTS` before a fresh `CREATE`), which is correct whether the table
/// never existed, is the pre-external-content standalone shape, or already
/// matches this exact shape. `IF NOT EXISTS` throughout 0001 means replaying
/// it against a pre-migration db (`user_version` still 0) is a harmless
/// no-op. Future schema changes are new `000N_*.sql` files appended here,
/// never edits to these.
const MIGRATION_LIST: &[M<'static>] = &[
    M::up(include_str!("migrations/0001_initial.sql")),
    M::up(include_str!("migrations/0002_fts_triggers.sql")),
];
const MIGRATIONS: Migrations = Migrations::from_slice(MIGRATION_LIST);

pub fn open_db(path: &Path) -> Result<Connection, db_kit::open::Error> {
    // `db_kit::open_file` already sets a busy_timeout and WAL journal mode.
    db_kit::open_file(path, &MIGRATIONS)
}

pub fn open_in_memory() -> Result<Connection, db_kit::open::Error> {
    db_kit::open_memory(&MIGRATIONS)
}

/// One backend's discovered tools, tagged with the server name they came from.
pub struct ServerTools {
    pub server: String,
    pub tools: Vec<ToolEntry>,
}

pub fn rebuild(conn: &mut Connection, entries: &[ServerTools]) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    // tools_fts follows along through the AFTER DELETE / AFTER INSERT
    // triggers — including the OR IGNORE case below, where no row is
    // inserted and so nothing is indexed.
    tx.execute("DELETE FROM tools", [])?;
    {
        let mut ins = tx.prepare(
            "INSERT OR IGNORE INTO tools (server, name, description, input_schema)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for st in entries {
            for t in &st.tools {
                let schema_json =
                    serde_json::to_string(&t.input_schema).unwrap_or_else(|_| "{}".into());
                ins.execute(params![st.server, t.name, t.description, schema_json])?;
            }
        }
    }
    tx.commit()
}

/// Tool names known for one server, for fuzzy-suggestion lookups in
/// `Registry::execute` (Task 9) without a live downstream round-trip.
pub fn tool_names(conn: &Connection, server: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT name FROM tools WHERE server = ?1 ORDER BY name")?;
    let rows = stmt.query_map(params![server], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// Full previously-indexed tool entries (name, description, input_schema)
/// for one server, as of the last successful `rebuild`. Used by
/// `Registry::ensure_fresh` to fall back to a server's last-known-good tool
/// list when that server's live `discover()` fails on a given refresh —
/// `rebuild` is a full-replace, so anything not re-contributed on a given
/// refresh would otherwise vanish from the index even on a purely
/// transient failure.
pub fn server_tools(conn: &Connection, server: &str) -> rusqlite::Result<Vec<ToolEntry>> {
    let mut stmt = conn.prepare(
        "SELECT name, description, input_schema FROM tools WHERE server = ?1 ORDER BY name",
    )?;
    let rows = stmt.query_map(params![server], |r| {
        let schema_json: String = r.get(2)?;
        let input_schema: serde_json::Value =
            serde_json::from_str(&schema_json).unwrap_or(serde_json::Value::Null);
        Ok(ToolEntry {
            name: r.get(0)?,
            description: r.get(1)?,
            input_schema,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rows the index can actually reach. Deliberately not
    /// `SELECT count(*) FROM tools_fts`: now that the table is
    /// external-content, a bare scan is answered from `tools` itself, so it
    /// reports every tool row whether or not it is indexed. Only a MATCH goes
    /// through the index.
    fn fts_hits(conn: &Connection, term: &str) -> i64 {
        conn.query_row(
            "SELECT count(*) FROM tools_fts WHERE tools_fts MATCH ?1",
            params![term],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn entry(name: &str, desc: &str) -> ToolEntry {
        ToolEntry {
            name: name.into(),
            description: desc.into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    #[test]
    fn rebuild_replaces_rows_and_fts_stays_in_sync() {
        let mut conn = open_in_memory().unwrap();
        rebuild(
            &mut conn,
            &[ServerTools {
                server: "narsil".into(),
                tools: vec![
                    entry("find_symbols", "alpha desc"),
                    entry("references", "beta desc"),
                ],
            }],
        )
        .unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM tools", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
        rebuild(
            &mut conn,
            &[ServerTools {
                server: "narsil".into(),
                tools: vec![entry("gamma", "gamma desc")],
            }],
        )
        .unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM tools", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(fts_hits(&conn, "gamma"), 1);
        assert_eq!(
            fts_hits(&conn, "alpha"),
            0,
            "a tool dropped by the rebuild must not stay matchable"
        );
    }

    #[test]
    fn fts_rowids_match_tools_rowids() {
        let mut conn = open_in_memory().unwrap();
        rebuild(
            &mut conn,
            &[ServerTools {
                server: "s".into(),
                tools: vec![entry("a", "alpha")],
            }],
        )
        .unwrap();
        let pair: (i64, i64) = conn
            .query_row(
                "SELECT t.rowid, f.rowid FROM tools t, tools_fts f WHERE f.name = t.name",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(pair.0, pair.1);
    }

    #[test]
    fn tool_names_scoped_to_server() {
        let mut conn = open_in_memory().unwrap();
        rebuild(
            &mut conn,
            &[
                ServerTools {
                    server: "a".into(),
                    tools: vec![entry("x", "")],
                },
                ServerTools {
                    server: "b".into(),
                    tools: vec![entry("y", "")],
                },
            ],
        )
        .unwrap();
        assert_eq!(tool_names(&conn, "a").unwrap(), vec!["x".to_string()]);
        assert_eq!(tool_names(&conn, "b").unwrap(), vec!["y".to_string()]);
        assert!(tool_names(&conn, "missing").unwrap().is_empty());
    }

    #[test]
    fn server_tools_returns_full_entries_scoped_to_server() {
        let mut conn = open_in_memory().unwrap();
        rebuild(
            &mut conn,
            &[
                ServerTools {
                    server: "a".into(),
                    tools: vec![entry("x", "desc-x")],
                },
                ServerTools {
                    server: "b".into(),
                    tools: vec![entry("y", "desc-y")],
                },
            ],
        )
        .unwrap();
        let a_tools = server_tools(&conn, "a").unwrap();
        assert_eq!(a_tools.len(), 1);
        assert_eq!(a_tools[0].name, "x");
        assert_eq!(a_tools[0].description, "desc-x");
        assert_eq!(
            a_tools[0].input_schema,
            serde_json::json!({"type": "object"})
        );
        assert!(server_tools(&conn, "missing").unwrap().is_empty());
    }

    #[test]
    fn a_legacy_standalone_fts_is_converted_and_refilled_on_open() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("gateway.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE tools (
                   server TEXT NOT NULL, name TEXT NOT NULL,
                   description TEXT NOT NULL DEFAULT '',
                   input_schema TEXT NOT NULL DEFAULT '{}',
                   PRIMARY KEY (server, name));
                 CREATE VIRTUAL TABLE tools_fts USING fts5(server, name, description);
                 INSERT INTO tools (server, name, description) VALUES ('narsil', 'find_symbols', 'alpha');
                 INSERT INTO tools_fts(rowid, server, name, description)
                    VALUES (1, 'narsil', 'find_symbols', 'alpha');",
            )
            .unwrap();
        }

        let conn = open_db(&path).unwrap();

        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'tools_fts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(sql.contains("content='tools'"), "must convert: {sql}");
        // The fallback tool list Registry::ensure_fresh leans on has to stay
        // searchable across the upgrade, not just after the next rebuild.
        let hits = crate::search::search(&conn, "alpha", 5, crate::search::MatchMode::All).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].tool, "find_symbols");
    }

    #[test]
    fn open_db_sets_wal_journal_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("gateway.db")).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }
}
