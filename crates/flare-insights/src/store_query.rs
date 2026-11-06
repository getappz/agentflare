//! SQL-side query helpers for InsightsStore.
//!
//! `sessions.source` / `sessions.status` are stored via `serde_json::to_string`,
//! so filters must compare against the JSON-quoted form (e.g. `"codex"`).

use rusqlite::params_from_iter;
use rusqlite::types::Value;
use serde::Serialize;

use crate::model::Session;
use crate::store::{row_to_session, InsightsStore, StoreError};

/// Exact string form used when upserting a JSON column (`serde_json::to_string`).
pub fn column_json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("serialize JSON column")
}

/// Map a logical source name (`"codex"`) to every on-disk JSON column value it may
/// have been stored as, including legacy aliases (`"open_code"`, `"open_claw"`).
pub fn source_columns(logical: &str) -> Vec<String> {
    let aliases: &[&str] = match logical {
        "opencode" => &["open_code"],
        "openclaw" => &["open_claw"],
        _ => &[],
    };
    std::iter::once(logical)
        .chain(aliases.iter().copied())
        .map(|s| column_json(&s))
        .collect()
}

impl InsightsStore {
    /// List sessions with optional source and project-prefix filters applied in SQL.
    ///
    /// Project scope matches `cwd` or `project_path` equal to `prefix`, or starting
    /// with `prefix` + `/` (so `/h/proj` does not match `/h/proj2`).
    pub fn list_sessions_filtered(
        &self,
        source: Option<&str>,
        project_prefix: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Session>, StoreError> {
        let mut sql = String::from("SELECT * FROM sessions WHERE 1=1");
        let mut binds: Vec<Value> = Vec::new();

        if let Some(src) = source {
            let cols = source_columns(src);
            let marks = vec!["?"; cols.len()].join(",");
            sql.push_str(&format!(" AND source IN ({marks})"));
            binds.extend(cols.into_iter().map(Value::Text));
        }

        if let Some(prefix) = project_prefix {
            // Literal, case-sensitive comparison (LIKE would treat `_`/`%` as wildcards
            // and fold ASCII case). Trailing separators are ignored; children may use
            // `/` or `\\`.
            let base = prefix.trim_end_matches(['/', '\\']);
            let n = base.chars().count() as i64 + 1;
            let mut clause = String::from(" AND (");
            for (i, col) in ["cwd", "project_path"].iter().enumerate() {
                if i > 0 {
                    clause.push_str(" OR ");
                }
                clause.push_str(&format!("{col} = ? OR (substr({col}, 1, ?) IN (?, ?))"));
                binds.push(Value::Text(base.to_string()));
                binds.push(Value::Integer(n));
                binds.push(Value::Text(format!("{base}/")));
                binds.push(Value::Text(format!("{base}\\")));
            }
            clause.push(')');
            sql.push_str(&clause);
        }

        sql.push_str(" ORDER BY updated_at DESC LIMIT ? OFFSET ?");
        binds.push(Value::Integer(limit as i64));
        binds.push(Value::Integer(offset as i64));

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(binds), row_to_session)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use chrono::Utc;

    fn sample_session(id: &str, source: SessionSource, cwd: Option<&str>) -> Session {
        Session {
            id: id.into(),
            source,
            project: "demo".into(),
            project_path: cwd.map(str::to_string),
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
            cost_basis: CostBasis::Unknown,
            pricing_key: None,
            turn_count: 0,
            tool_call_count: 0,
            subagent_count: 0,
            tags: vec![],
            starred: false,
            pid: None,
            cwd: cwd.map(str::to_string),
        }
    }

    #[test]
    fn source_filter_hits_inserted_claude_row() {
        let store = InsightsStore::open_in_memory().unwrap();
        store
            .upsert_session(&sample_session("s1", SessionSource::ClaudeCode, None))
            .unwrap();

        let hit = store
            .list_sessions_filtered(Some("claude_code"), None, 10, 0)
            .unwrap();
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].id, "s1");

        let miss = store
            .list_sessions_filtered(Some("codex"), None, 10, 0)
            .unwrap();
        assert_eq!(miss.len(), 0);
    }

    #[test]
    fn project_prefix_does_not_match_sibling_dir() {
        let store = InsightsStore::open_in_memory().unwrap();
        store
            .upsert_session(&sample_session(
                "sib",
                SessionSource::Codex,
                Some("/h/proj2"),
            ))
            .unwrap();
        store
            .upsert_session(&sample_session(
                "self",
                SessionSource::Codex,
                Some("/h/proj"),
            ))
            .unwrap();
        store
            .upsert_session(&sample_session(
                "child",
                SessionSource::Codex,
                Some("/h/proj/sub"),
            ))
            .unwrap();

        let rows = store
            .list_sessions_filtered(None, Some("/h/proj"), 10, 0)
            .unwrap();
        let mut ids: Vec<_> = rows.iter().map(|s| s.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["child", "self"]);
    }

    #[test]
    fn source_filter_matches_legacy_and_irregular_names() {
        let store = InsightsStore::open_in_memory().unwrap();
        store
            .upsert_session(&sample_session("oc", SessionSource::OpenCode, None))
            .unwrap();
        store
            .upsert_session(&sample_session("claw", SessionSource::OpenClaw, None))
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE sessions SET source = '\"open_code\"' WHERE id = 'oc'",
                [],
            )
            .unwrap();
        let oc = store
            .list_sessions_filtered(Some("opencode"), None, 10, 0)
            .unwrap();
        assert_eq!(oc.len(), 1);
        let claw = store
            .list_sessions_filtered(Some("openclaw"), None, 10, 0)
            .unwrap();
        assert_eq!(claw.len(), 1);
    }

    #[test]
    fn project_prefix_is_literal_case_sensitive_and_ignores_trailing_sep() {
        let store = InsightsStore::open_in_memory().unwrap();
        for (id, cwd) in [
            ("wild", "/w/myXproj/sub"),
            ("case", "/w/MY_PROJ"),
            ("hit", "/w/my_proj/sub"),
        ] {
            store
                .upsert_session(&sample_session(id, SessionSource::Codex, Some(cwd)))
                .unwrap();
        }
        for prefix in ["/w/my_proj", "/w/my_proj/"] {
            let rows = store
                .list_sessions_filtered(None, Some(prefix), 10, 0)
                .unwrap();
            let ids: Vec<_> = rows.iter().map(|s| s.id.as_str()).collect();
            assert_eq!(ids, vec!["hit"], "prefix {prefix}");
        }
    }
}
