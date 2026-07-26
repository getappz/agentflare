//! SQLite persistence: tools table + FTS5 index. Both are derived data —
//! live `tools/list` responses from connected downstream servers are the
//! source of truth, so rebuild is always full-replace inside one
//! transaction (same pattern as `crates/skill-registry/src/db.rs`).

use crate::types::ToolEntry;
use rusqlite::{Connection, params};
use std::path::Path;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tools (
  server TEXT NOT NULL,
  name TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  input_schema TEXT NOT NULL DEFAULT '{}',
  PRIMARY KEY (server, name)
);
CREATE VIRTUAL TABLE IF NOT EXISTS tools_fts USING fts5(
  server, name, description, content='tools'
);
CREATE TRIGGER IF NOT EXISTS tools_fts_ai AFTER INSERT ON tools BEGIN
  INSERT INTO tools_fts(rowid, server, name, description)
  VALUES (new.rowid, new.server, new.name, new.description);
END;
CREATE TRIGGER IF NOT EXISTS tools_fts_ad AFTER DELETE ON tools BEGIN
  INSERT INTO tools_fts(tools_fts, rowid, server, name, description)
  VALUES ('delete', old.rowid, old.server, old.name, old.description);
END;
CREATE TRIGGER IF NOT EXISTS tools_fts_au
AFTER UPDATE OF server, name, description ON tools BEGIN
  INSERT INTO tools_fts(tools_fts, rowid, server, name, description)
  VALUES ('delete', old.rowid, old.server, old.name, old.description);
  INSERT INTO tools_fts(rowid, server, name, description)
  VALUES (new.rowid, new.server, new.name, new.description);
END;
";

/// Databases written before `tools_fts` became external-content carry the old
/// standalone table, which `CREATE VIRTUAL TABLE IF NOT EXISTS` would leave
/// in place. Drop it so `SCHEMA` recreates the trigger-backed shape, then
/// refill from `tools` -- the last-known-good tool list `Registry` falls back
/// on must stay searchable across the upgrade, not just after the next
/// `rebuild`. (Mirrors `crates/skill-registry/src/db.rs`.)
///
/// All of it in one transaction, because the halfway state is not
/// self-correcting: with `tools_fts` dropped but not yet refilled, the next
/// open finds no such table, reads `legacy` as 0, and recreates an empty
/// index over a populated `tools` — search silently missing every tool until
/// something forces a full `rebuild`. SQLite makes DDL transactional, so the
/// conversion either lands whole or never happened.
fn apply_schema(conn: &Connection) -> rusqlite::Result<()> {
    let legacy: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master
          WHERE type = 'table' AND name = 'tools_fts' AND sql NOT LIKE '%content=%'",
        [],
        |r| r.get(0),
    )?;
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    if legacy > 0 {
        tx.execute_batch("DROP TABLE tools_fts;")?;
    }
    tx.execute_batch(SCHEMA)?;
    if legacy > 0 {
        tx.execute_batch(
            "INSERT INTO tools_fts(rowid, server, name, description)
             SELECT rowid, server, name, description FROM tools;",
        )?;
    }
    tx.commit()
}

pub fn open_db(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)?;
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
