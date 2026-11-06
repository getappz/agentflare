//! SQL-side query helpers for InsightsStore.
//!
//! `sessions.source` / `sessions.status` are stored via `serde_json::to_string`,
//! so filters must compare against the JSON-quoted form (e.g. `"codex"`).

use chrono::{DateTime, Utc};
use rusqlite::params_from_iter;
use rusqlite::types::Value;
use serde::{Deserialize, Serialize};

use crate::model::Session;
use crate::store::{row_to_session, InsightsStore, StoreError};

/// One search result: session plus the best-matching turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub session: Session,
    pub turn_seq: u32,
    pub rank: f64,
    pub snippet: String,
}

/// Filters applied in SQL before grouping / limiting search hits.
#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    pub source: Option<String>,
    pub project_prefix: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub limit: usize,
    pub offset: usize,
}

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

fn bound_snippet(s: String) -> String {
    if s.chars().count() <= 240 {
        s
    } else {
        s.chars().take(240).collect()
    }
}

/// Append source / project-prefix / since predicates for a `sessions` alias `s`.
fn push_session_filters(
    sql: &mut String,
    binds: &mut Vec<Value>,
    source: Option<&str>,
    project_prefix: Option<&str>,
    since: Option<DateTime<Utc>>,
) {
    if let Some(src) = source {
        let cols = source_columns(src);
        let marks = vec!["?"; cols.len()].join(",");
        sql.push_str(&format!(" AND s.source IN ({marks})"));
        binds.extend(cols.into_iter().map(Value::Text));
    }

    if let Some(prefix) = project_prefix {
        // Literal, case-sensitive comparison (LIKE would treat `_`/`%` as wildcards
        // and fold ASCII case). Trailing separators are ignored; children may use
        // `/` or `\\`.
        let base = prefix.trim_end_matches(['/', '\\']);
        let n = base.chars().count() as i64 + 1;
        let mut clause = String::from(" AND (");
        for (i, col) in ["s.cwd", "s.project_path"].iter().enumerate() {
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

    if let Some(since) = since {
        sql.push_str(" AND s.updated_at >= ?");
        binds.push(Value::Text(since.to_rfc3339()));
    }
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
        let mut sql = String::from("SELECT s.* FROM sessions s WHERE 1=1");
        let mut binds: Vec<Value> = Vec::new();
        push_session_filters(&mut sql, &mut binds, source, project_prefix, None);

        sql.push_str(" ORDER BY s.updated_at DESC LIMIT ? OFFSET ?");
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

    /// Full-text (or short-query LIKE) search with filters applied before limit.
    ///
    /// One hit per session (best `rank`); queries shorter than 3 chars use LIKE.
    pub fn search_hits(
        &self,
        query: &str,
        f: &SearchFilters,
    ) -> Result<Vec<SearchHit>, StoreError> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }

        let use_like = q.chars().count() < 3;
        let mut binds: Vec<Value> = Vec::new();
        let mut sql = if use_like {
            let pattern = format!("%{q}%");
            binds.push(Value::Text(pattern.clone()));
            binds.push(Value::Text(pattern.clone()));
            binds.push(Value::Text(pattern));
            String::from(
                r#"WITH matched AS (
                    SELECT
                        t.session_id AS sid,
                        t.seq AS turn_seq,
                        0.0 AS hit_rank,
                        CASE
                            WHEN t.assistant_text LIKE ? THEN substr(COALESCE(t.assistant_text, ''), 1, 240)
                            ELSE substr(COALESCE(t.user_text, ''), 1, 240)
                        END AS snip,
                        ROW_NUMBER() OVER (PARTITION BY t.session_id ORDER BY t.seq) AS rn
                    FROM turns t
                    JOIN sessions s ON s.id = t.session_id
                    WHERE (t.user_text LIKE ? OR t.assistant_text LIKE ?)"#,
            )
        } else {
            // snippet() requires MATCH in the same SELECT; pick best rowids in a CTE
            // first, then join back for snippet.
            let fts_q = flare_search_kit::fts_phrase_query(q);
            binds.push(Value::Text(fts_q.clone()));
            String::from(
                r#"WITH candidates AS (
                    SELECT
                        t.rowid AS turn_rowid,
                        t.session_id AS sid,
                        t.seq AS turn_seq,
                        rank AS hit_rank,
                        ROW_NUMBER() OVER (
                            PARTITION BY t.session_id ORDER BY rank, t.rowid
                        ) AS rn
                    FROM turns_fts
                    JOIN turns t ON t.rowid = turns_fts.rowid
                    JOIN sessions s ON s.id = t.session_id
                    WHERE turns_fts MATCH ?"#,
            )
        };

        push_session_filters(
            &mut sql,
            &mut binds,
            f.source.as_deref(),
            f.project_prefix.as_deref(),
            f.since,
        );

        if use_like {
            sql.push_str(
                r#"
                )
                SELECT s.*, m.turn_seq, m.hit_rank AS rank, m.snip AS snippet
                FROM matched m
                JOIN sessions s ON s.id = m.sid
                WHERE m.rn = 1
                ORDER BY m.hit_rank
                LIMIT ?, ?"#,
            );
        } else {
            let fts_q = flare_search_kit::fts_phrase_query(q);
            binds.push(Value::Text(fts_q));
            sql.push_str(
                r#"
                ),
                best AS (
                    SELECT turn_rowid, sid, turn_seq, hit_rank
                    FROM candidates WHERE rn = 1
                )
                SELECT s.*, b.turn_seq, b.hit_rank AS rank,
                       snippet(turns_fts, 2, '[', ']', '…', 24) AS snippet
                FROM best b
                JOIN turns_fts ON turns_fts.rowid = b.turn_rowid
                JOIN sessions s ON s.id = b.sid
                WHERE turns_fts MATCH ?
                ORDER BY b.hit_rank
                LIMIT ?, ?"#,
            );
        }
        binds.push(Value::Integer(f.offset as i64));
        binds.push(Value::Integer(f.limit as i64));

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(binds), |row| {
            let session = row_to_session(row)?;
            let turn_seq: i64 = row.get("turn_seq")?;
            let rank: f64 = row.get("rank")?;
            let snippet: String = row.get("snippet")?;
            Ok(SearchHit {
                session,
                turn_seq: turn_seq as u32,
                rank,
                snippet: bound_snippet(snippet),
            })
        })?;
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
