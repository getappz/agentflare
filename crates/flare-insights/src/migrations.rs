//! Versioned schema for the insights index (`PRAGMA user_version`).
//!
//! `V1_DDL` is the frozen pre-migration schema. Never edit it; every schema
//! change is a new `M::up` entry appended to `migrations()`.

use rusqlite_migration::{Migrations, M};

/// Frozen v1 DDL — byte-identical to the original `CREATE TABLE IF NOT EXISTS`
/// batch (minus the journal/foreign-key pragmas, which run outside migrations).
pub const V1_DDL: &str = r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                source TEXT NOT NULL,
                project TEXT NOT NULL,
                project_path TEXT,
                title TEXT,
                model TEXT,
                status TEXT NOT NULL,
                awaiting_reason TEXT,
                started_at TEXT,
                updated_at TEXT NOT NULL,
                ended_at TEXT,
                duration_secs INTEGER,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                reasoning_tokens INTEGER NOT NULL DEFAULT 0,
                cost_usd REAL,
                turn_count INTEGER NOT NULL DEFAULT 0,
                tool_call_count INTEGER NOT NULL DEFAULT 0,
                subagent_count INTEGER NOT NULL DEFAULT 0,
                tags TEXT NOT NULL DEFAULT '[]',
                starred INTEGER NOT NULL DEFAULT 0,
                pid INTEGER,
                cwd TEXT
            );

            CREATE TABLE IF NOT EXISTS turns (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq INTEGER NOT NULL,
                user_text TEXT,
                assistant_text TEXT,
                started_at TEXT,
                ended_at TEXT,
                input_tokens INTEGER,
                output_tokens INTEGER,
                cost_usd REAL,
                UNIQUE(session_id, seq)
            );

            CREATE TABLE IF NOT EXISTS tool_calls (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                turn_seq INTEGER NOT NULL,
                name TEXT NOT NULL,
                input TEXT NOT NULL,
                output TEXT,
                status TEXT NOT NULL,
                duration_ms INTEGER,
                created_at TEXT
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS turns_fts USING fts5(
                session_id, user_text, assistant_text,
                content='turns', content_rowid='rowid',
                tokenize='trigram'
            );
            CREATE TRIGGER IF NOT EXISTS turns_ai AFTER INSERT ON turns BEGIN
                INSERT INTO turns_fts(rowid, session_id, user_text, assistant_text)
                VALUES (new.rowid, new.session_id, new.user_text, new.assistant_text);
            END;
            CREATE TRIGGER IF NOT EXISTS turns_ad AFTER DELETE ON turns BEGIN
                INSERT INTO turns_fts(turns_fts, rowid, session_id, user_text, assistant_text)
                VALUES('delete', old.rowid, old.session_id, old.user_text, old.assistant_text);
            END;
            CREATE TRIGGER IF NOT EXISTS turns_au AFTER UPDATE ON turns BEGIN
                INSERT INTO turns_fts(turns_fts, rowid, session_id, user_text, assistant_text)
                VALUES('delete', old.rowid, old.session_id, old.user_text, old.assistant_text);
                INSERT INTO turns_fts(rowid, session_id, user_text, assistant_text)
                VALUES (new.rowid, new.session_id, new.user_text, new.assistant_text);
            END;

            CREATE TABLE IF NOT EXISTS file_events (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                path TEXT NOT NULL,
                kind TEXT NOT NULL,
                at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS subagents (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                parent_tool_call_id TEXT,
                kind TEXT NOT NULL,
                status TEXT NOT NULL,
                task TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at);
            CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project);
            CREATE INDEX IF NOT EXISTS idx_sessions_source ON sessions(source);
            CREATE INDEX IF NOT EXISTS idx_turns_session ON turns(session_id);
            CREATE INDEX IF NOT EXISTS idx_tool_calls_session ON tool_calls(session_id);
            CREATE INDEX IF NOT EXISTS idx_tool_calls_name ON tool_calls(name);
            CREATE INDEX IF NOT EXISTS idx_file_events_session ON file_events(session_id);
            CREATE INDEX IF NOT EXISTS idx_file_events_path ON file_events(path);

            CREATE TABLE IF NOT EXISTS ingest_file_cursors (
                source TEXT NOT NULL,
                path TEXT NOT NULL,
                mtime_ms INTEGER NOT NULL,
                size INTEGER NOT NULL,
                PRIMARY KEY (source, path)
            );
            "#;

/// Ordered migrations; `user_version` after `to_latest` equals the list length.
pub fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(V1_DDL),
        M::up(
            "ALTER TABLE turns ADD COLUMN cache_read_tokens INTEGER;
             ALTER TABLE turns ADD COLUMN cache_write_tokens INTEGER;
             ALTER TABLE turns ADD COLUMN reasoning_tokens INTEGER;
             ALTER TABLE sessions ADD COLUMN cost_basis TEXT;
             ALTER TABLE sessions ADD COLUMN pricing_key TEXT;",
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use crate::store::InsightsStore;
    use chrono::{TimeZone, Utc};
    use rusqlite::Connection;

    fn session(id: &str) -> Session {
        Session {
            id: id.into(),
            source: SessionSource::ClaudeCode,
            project: "demo".into(),
            project_path: None,
            title: None,
            model: None,
            status: SessionStatus::Completed,
            awaiting_reason: None,
            started_at: None,
            updated_at: Utc::now(),
            ended_at: None,
            duration_secs: None,
            tokens: TokenUsage {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            cost: None,
            cost_basis: crate::model::CostBasis::Unknown,
            pricing_key: None,
            turn_count: 0,
            tool_call_count: 0,
            subagent_count: 0,
            tags: vec![],
            starred: false,
            pid: None,
            cwd: None,
        }
    }

    #[test]
    fn session_cost_basis_round_trips_as_plain_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("insights.db");
        let store = InsightsStore::open(&path).unwrap();
        let raw = Connection::open(&path).unwrap();
        let mut s = session("priced");
        for (basis, text, key) in [
            (CostBasis::Provider, "provider", None),
            (
                CostBasis::Estimated,
                "estimated",
                Some("claude-sonnet-4".to_string()),
            ),
            (CostBasis::Unknown, "unknown", None),
        ] {
            s.cost_basis = basis;
            s.pricing_key = key.clone();
            store.upsert_session(&s).unwrap();
            let loaded = store.get_session(&s.id).unwrap().unwrap();
            assert_eq!(loaded.cost_basis, basis);
            assert_eq!(loaded.pricing_key, key);
            let stored: String = raw
                .query_row(
                    "SELECT cost_basis FROM sessions WHERE id='priced'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(stored, text);
        }
        raw.execute("UPDATE sessions SET cost_basis=NULL WHERE id='priced'", [])
            .unwrap();
        assert_eq!(
            store.get_session(&s.id).unwrap().unwrap().cost_basis,
            CostBasis::Unknown
        );
    }

    #[test]
    fn old_v1_db_upgrades_without_losing_rows_or_fts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("insights.db");
        {
            let raw = Connection::open(&path).unwrap();
            raw.execute_batch(V1_DDL).unwrap();
            raw.execute_batch(
                r#"INSERT INTO sessions(id, source, project, status, updated_at)
                   VALUES ('s1', '"claude_code"', 'demo', '"completed"', '2026-01-01T00:00:00+00:00');
                   INSERT INTO turns(id, session_id, seq, user_text, input_tokens, output_tokens)
                   VALUES ('t1', 's1', 1, 'hello world', 10, 5);
                   INSERT INTO turns(id, session_id, seq, user_text)
                   VALUES ('t2', 's1', 2, 'second turn');"#,
            )
            .unwrap();
        }

        let store = InsightsStore::open(&path).unwrap();
        let turns = store.get_turns("s1").unwrap();
        assert_eq!(turns.len(), 2);
        let tok = turns[0].tokens.as_ref().expect("v1 row keeps its tokens");
        assert_eq!((tok.input, tok.output, tok.cache_read), (10, 5, 0));
        assert!(turns[1].tokens.is_none());
        let hits = store.search_sessions("hello", 10).unwrap();
        assert_eq!(hits.len(), 1, "FTS index survives the upgrade");
        assert_eq!(hits[0].id, "s1");
        drop(store);

        let raw = Connection::open(&path).unwrap();
        let version: i64 = raw
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 2);
    }

    #[test]
    fn turn_round_trip_preserves_cache_and_timestamps() {
        let store = InsightsStore::open_in_memory().unwrap();
        store.upsert_session(&session("s1")).unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 10, 0, 0).unwrap();
        let t1 = Utc.with_ymd_and_hms(2026, 1, 1, 10, 5, 0).unwrap();
        let mut turn = Turn {
            id: "t1".into(),
            session_id: "s1".into(),
            seq: 1,
            user_text: Some("hi".into()),
            assistant_text: None,
            started_at: Some(t0),
            ended_at: Some(t1),
            tokens: Some(TokenUsage {
                input: 100,
                output: 20,
                cache_read: 7,
                cache_write: 3,
                reasoning: 1,
            }),
            cost_usd: None,
        };
        store
            .upsert_turns_batch(std::slice::from_ref(&turn))
            .unwrap();

        let got = store.get_turns("s1").unwrap();
        let tok = got[0].tokens.as_ref().unwrap();
        assert_eq!((tok.cache_read, tok.cache_write, tok.reasoning), (7, 3, 1));
        assert_eq!((got[0].started_at, got[0].ended_at), (Some(t0), Some(t1)));

        let t2 = Utc.with_ymd_and_hms(2026, 1, 1, 11, 0, 0).unwrap();
        let t3 = Utc.with_ymd_and_hms(2026, 1, 1, 11, 9, 0).unwrap();
        turn.started_at = Some(t2);
        turn.ended_at = Some(t3);
        turn.tokens.as_mut().unwrap().cache_read = 9;
        store
            .upsert_turns_batch(std::slice::from_ref(&turn))
            .unwrap();

        let got = store.get_turns("s1").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].started_at, got[0].ended_at), (Some(t2), Some(t3)));
        assert_eq!(got[0].tokens.as_ref().unwrap().cache_read, 9);

        // Single-row path writes the same columns.
        turn.id = "t2".into();
        turn.seq = 2;
        store.upsert_turn(&turn).unwrap();
        let got = store.get_turns("s1").unwrap();
        assert_eq!(got[1].tokens.as_ref().unwrap().cache_write, 3);
    }
}
