//! SQLite persistence: tools table + FTS5 index. Both are derived data —
//! live `tools/list` responses from connected downstream servers are the
//! source of truth, so rebuild is always full-replace inside one
//! transaction (same pattern as `crates/skill-registry/src/db.rs`).

use crate::types::ToolEntry;
use rusqlite::{params, Connection};
use std::path::Path;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tools (
  server TEXT NOT NULL,
  name TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  input_schema TEXT NOT NULL DEFAULT '{}',
  PRIMARY KEY (server, name)
);
CREATE VIRTUAL TABLE IF NOT EXISTS tools_fts USING fts5(server, name, description);
";

pub fn open_db(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)?;
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

/// One backend's discovered tools, tagged with the server name they came from.
pub struct ServerTools {
    pub server: String,
    pub tools: Vec<ToolEntry>,
}

pub fn rebuild(conn: &mut Connection, entries: &[ServerTools]) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM tools", [])?;
    tx.execute("DELETE FROM tools_fts", [])?;
    {
        let mut ins = tx.prepare(
            "INSERT OR IGNORE INTO tools (server, name, description, input_schema)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        let mut fts = tx.prepare(
            "INSERT INTO tools_fts (rowid, server, name, description) VALUES (?1, ?2, ?3, ?4)",
        )?;
        for st in entries {
            for t in &st.tools {
                let schema_json =
                    serde_json::to_string(&t.input_schema).unwrap_or_else(|_| "{}".into());
                ins.execute(params![st.server, t.name, t.description, schema_json])?;
                if tx.changes() == 0 {
                    // Duplicate (server, name) ignored — skip the fts mirror,
                    // last_insert_rowid() would be stale (mirrors skill-registry).
                    continue;
                }
                let rowid = tx.last_insert_rowid();
                fts.execute(params![rowid, st.server, t.name, t.description])?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
                tools: vec![entry("find_symbols", "alpha desc"), entry("references", "beta desc")],
            }],
        )
        .unwrap();
        let n: i64 = conn.query_row("SELECT count(*) FROM tools", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
        rebuild(
            &mut conn,
            &[ServerTools { server: "narsil".into(), tools: vec![entry("gamma", "gamma desc")] }],
        )
        .unwrap();
        let n: i64 = conn.query_row("SELECT count(*) FROM tools", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let f: i64 = conn.query_row("SELECT count(*) FROM tools_fts", [], |r| r.get(0)).unwrap();
        assert_eq!(f, 1);
    }

    #[test]
    fn fts_rowids_match_tools_rowids() {
        let mut conn = open_in_memory().unwrap();
        rebuild(&mut conn, &[ServerTools { server: "s".into(), tools: vec![entry("a", "alpha")] }])
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
                ServerTools { server: "a".into(), tools: vec![entry("x", "")] },
                ServerTools { server: "b".into(), tools: vec![entry("y", "")] },
            ],
        )
        .unwrap();
        assert_eq!(tool_names(&conn, "a").unwrap(), vec!["x".to_string()]);
        assert_eq!(tool_names(&conn, "b").unwrap(), vec!["y".to_string()]);
        assert!(tool_names(&conn, "missing").unwrap().is_empty());
    }

    #[test]
    fn open_db_sets_wal_journal_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("gateway.db")).unwrap();
        let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(mode, "wal");
    }
}
