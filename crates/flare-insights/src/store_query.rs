//! SQL-side query helpers for InsightsStore.
//!
//! `sessions.source` / `sessions.status` are stored via `serde_json::to_string`,
//! so filters must compare against the JSON-quoted form (e.g. `"codex"`).

use chrono::{DateTime, Utc};
use rusqlite::params_from_iter;
use rusqlite::types::Value;
use serde::{Deserialize, Serialize};

use crate::analytics::{cache_metrics, Analytics, DailyBucket};
use crate::evidence::CoverageMeta;

use crate::model::{Session, SessionSource, TokenUsage};
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

/// `<select> WHERE 1=1 [AND ...]` for a query aliasing `sessions` as `s`.
fn sessions_select(
    select: &str,
    since: Option<DateTime<Utc>>,
    source: Option<&str>,
    project_prefix: Option<&str>,
) -> (String, Vec<Value>) {
    let mut sql = format!("{select} WHERE 1=1");
    let mut binds: Vec<Value> = Vec::new();
    push_session_filters(&mut sql, &mut binds, source, project_prefix, since);
    (sql, binds)
}

/// Window for [`InsightsStore::stats_window`].
#[derive(Debug, Clone, Default)]
pub struct StatsWindow {
    pub since: Option<DateTime<Utc>>,
    pub source: Option<String>,
    pub project_prefix: Option<String>,
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
        let (mut sql, mut binds) =
            sessions_select("SELECT s.* FROM sessions s", None, source, project_prefix);
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
                ORDER BY m.hit_rank, s.updated_at DESC, s.id
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
                       snippet(turns_fts, -1, '[', ']', '…', 24) AS snippet
                FROM best b
                JOIN turns_fts ON turns_fts.rowid = b.turn_rowid
                JOIN sessions s ON s.id = b.sid
                WHERE turns_fts MATCH ?
                ORDER BY b.hit_rank, s.updated_at DESC, s.id
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

    /// Every session updated since `since` (all when `None`), filtered in SQL.
    pub fn sessions_in_window(
        &self,
        since: Option<DateTime<Utc>>,
        source: Option<&str>,
        project_prefix: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<Vec<Session>, StoreError> {
        let (mut sql, mut binds) =
            sessions_select("SELECT s.* FROM sessions s", since, source, project_prefix);
        if let Some(id) = session_id {
            sql.push_str(" AND s.id = ?");
            binds.push(Value::Text(id.to_string()));
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(binds), row_to_session)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Aggregate stats over a window with SQL GROUP BY (no row-count cap).
    pub fn stats_window(&self, w: &StatsWindow) -> Result<Analytics, StoreError> {
        let (filter, binds) = sessions_select(
            "SELECT s.id FROM sessions s",
            w.since,
            w.source.as_deref(),
            w.project_prefix.as_deref(),
        );
        let ids = format!("session_id IN ({filter})");
        let conn = &self.conn;
        // Every read below sees one snapshot; dropping it just ends the read.
        let _snapshot = conn.unchecked_transaction()?;

        let mut a = Analytics {
            total_sessions: 0,
            total_tokens: 0,
            total_cost_usd: 0.0,
            by_source: Default::default(),
            by_model: Default::default(),
            by_project: Default::default(),
            by_day_updated: vec![],
            tool_freq: Default::default(),
            file_freq: Default::default(),
            tool_failure_rate: None,
            cache_read_share: None,
            cache_prompt_efficiency: None,
            coverage: CoverageMeta::default(),
        };

        let mut stmt = conn.prepare(&format!(
            "SELECT source, COUNT(*) FROM sessions WHERE id IN ({filter}) GROUP BY source"
        ))?;
        for r in stmt.query_map(params_from_iter(binds.clone()), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))
        })? {
            let (raw, n) = r?;
            let name = serde_json::from_str::<SessionSource>(&raw)
                .map(|s| s.as_str().to_string())
                .unwrap_or(raw);
            *a.by_source.entry(name).or_default() += n;
        }

        for (col, dest) in [("model", &mut a.by_model), ("project", &mut a.by_project)] {
            let mut stmt = conn.prepare(&format!(
                "SELECT {col}, COUNT(*) FROM sessions WHERE id IN ({filter}) AND {col} IS NOT NULL GROUP BY {col}"
            ))?;
            for r in stmt.query_map(params_from_iter(binds.clone()), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))
            })? {
                let (k, n) = r?;
                dest.insert(k, n);
            }
        }

        let mut stmt = conn.prepare(&format!(
            "SELECT substr(updated_at,1,10), COUNT(*), SUM(input_tokens), SUM(output_tokens),
                    SUM(cache_read_tokens), SUM(cache_write_tokens), SUM(reasoning_tokens),
                    COALESCE(SUM(cost_usd),0), SUM(cost_usd IS NULL)
             FROM sessions WHERE id IN ({filter}) GROUP BY 1 ORDER BY 1"
        ))?;
        let rows = stmt.query_map(params_from_iter(binds.clone()), |r| {
            let n = |i| r.get::<_, i64>(i).map(|v| v as u64);
            Ok((
                DailyBucket {
                    date: r.get(0)?,
                    sessions: n(1)? as usize,
                    tokens: TokenUsage {
                        input: n(2)?,
                        output: n(3)?,
                        cache_read: n(4)?,
                        cache_write: n(5)?,
                        reasoning: n(6)?,
                    },
                    cost_usd: r.get(7)?,
                },
                n(8)? as usize,
            ))
        })?;
        let (mut t_in, mut t_read, mut t_write) = (0, 0, 0);
        for r in rows {
            let (b, unpriced) = r?;
            a.total_sessions += b.sessions;
            a.total_tokens += b.tokens.total();
            a.total_cost_usd += b.cost_usd;
            t_in += b.tokens.input;
            t_read += b.tokens.cache_read;
            t_write += b.tokens.cache_write;
            a.coverage.unpriced_sessions += unpriced;
            a.by_day_updated.push(b);
        }
        a.coverage.sessions_in_window = a.total_sessions;
        a.coverage.eligible_sessions = a.total_sessions;
        let (first, last): (Option<String>, Option<String>) = conn.query_row(
            &format!(
                "SELECT MIN(updated_at), MAX(updated_at) FROM sessions WHERE id IN ({filter})"
            ),
            params_from_iter(binds.clone()),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let parse = |text: String| {
            DateTime::parse_from_rfc3339(&text)
                .map(|t| t.with_timezone(&Utc))
                .map_err(|e| StoreError::Parse(e.to_string()))
        };
        // An empty window with a future `since` must not end before it starts.
        a.coverage.window_end = last
            .map(parse)
            .transpose()?
            .unwrap_or_else(|| w.since.map_or_else(Utc::now, |s| s.max(Utc::now())));
        a.coverage.window_start = w
            .since
            .or(first.map(parse).transpose()?)
            .unwrap_or(a.coverage.window_end);
        a.coverage.eligible_turns = conn.query_row(
            &format!("SELECT COUNT(*) FROM turns WHERE {ids}"),
            params_from_iter(binds.clone()),
            |r| r.get::<_, i64>(0),
        )? as usize;
        a.coverage.sessions_missing_turn_timestamps = conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM sessions s WHERE id IN ({filter}) AND (
                    NOT EXISTS (SELECT 1 FROM turns t WHERE t.session_id = s.id)
                    OR EXISTS (SELECT 1 FROM turns t WHERE t.session_id = s.id
                               AND julianday(t.started_at) IS NULL))"
            ),
            params_from_iter(binds.clone()),
            |r| r.get::<_, i64>(0),
        )? as usize;
        (a.cache_read_share, a.cache_prompt_efficiency) = cache_metrics(t_in, t_read, t_write);

        let mut stmt = conn.prepare(&format!(
            "SELECT COALESCE(cost_basis, 'unknown'), COUNT(*) FROM sessions
             WHERE id IN ({filter}) AND cost_usd IS NOT NULL GROUP BY 1"
        ))?;
        for r in stmt.query_map(params_from_iter(binds.clone()), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))
        })? {
            let (basis, n) = r?;
            a.coverage.cost_basis_mix.insert(basis, n);
        }

        let mut stmt = conn.prepare(&format!(
            "SELECT name, COUNT(*), SUM(status IN ('failed','error'))
             FROM tool_calls WHERE {ids} GROUP BY name"
        ))?;
        let (mut calls, mut failed) = (0u64, 0u64);
        for r in stmt.query_map(params_from_iter(binds.clone()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)? as u64,
                r.get::<_, i64>(2)? as u64,
            ))
        })? {
            let (name, n, f) = r?;
            calls += n;
            failed += f;
            a.tool_freq.insert(name, n);
        }
        a.tool_failure_rate = (calls > 0).then(|| failed as f64 / calls as f64);

        let mut stmt = conn.prepare(&format!(
            "SELECT path, COUNT(*) FROM file_events WHERE {ids} GROUP BY path"
        ))?;
        for r in stmt.query_map(params_from_iter(binds), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))
        })? {
            let (p, n) = r?;
            a.file_freq.insert(p, n);
        }
        Ok(a)
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

    fn with_tokens(mut s: Session, input: u64, read: u64) -> Session {
        s.tokens.input = input;
        s.tokens.cache_read = read;
        s
    }

    #[test]
    fn cache_metrics_none_when_no_input_tokens() {
        let store = InsightsStore::open_in_memory().unwrap();
        store
            .upsert_session(&sample_session("z", SessionSource::Cursor, None))
            .unwrap();
        let a = store.stats_window(&StatsWindow::default()).unwrap();
        assert_eq!(a.cache_read_share, None);
        assert_eq!(a.cache_prompt_efficiency, None);
        assert_eq!(a.coverage.sessions_in_window, 1);
        assert_eq!(a.coverage.unpriced_sessions, 1);
    }

    #[test]
    fn cursor_structural_zero_does_not_lower_share_when_excluded_by_source() {
        let store = InsightsStore::open_in_memory().unwrap();
        let claude = with_tokens(
            sample_session("c", SessionSource::ClaudeCode, None),
            100,
            300,
        );
        let cursor = with_tokens(sample_session("u", SessionSource::Cursor, None), 400, 0);
        store.upsert_sessions_batch(&[claude, cursor]).unwrap();
        let all = store.stats_window(&StatsWindow::default()).unwrap();
        assert_eq!(all.cache_read_share, Some(0.375));
        let only = store
            .stats_window(&StatsWindow {
                source: Some("claude_code".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(only.cache_read_share, Some(0.75));
        assert_eq!(only.by_source.get("claude_code"), Some(&1));
        assert_eq!(only.total_sessions, 1);
    }

    #[test]
    fn coverage_uses_filtered_turn_rows_and_absolute_bounds() {
        let store = InsightsStore::open_in_memory().unwrap();
        let start: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let end: DateTime<Utc> = "2026-01-02T00:00:00Z".parse().unwrap();
        for (id, source) in [
            ("timed", SessionSource::Codex),
            ("missing", SessionSource::Codex),
            ("empty", SessionSource::Codex),
            ("other", SessionSource::Cursor),
        ] {
            let mut session = sample_session(id, source, None);
            session.updated_at = end;
            // Session timestamps must not substitute for turn timestamps.
            session.started_at = Some(start);
            session.turn_count = 999;
            store.upsert_session(&session).unwrap();
        }
        store
            .conn
            .execute_batch(
                "INSERT INTO turns(id, session_id, seq, started_at) VALUES
                ('t1', 'timed', 1, '2026-01-01T12:00:00Z'),
                ('t2', 'missing', 1, NULL),
                ('t3', 'missing', 2, 'invalid'),
                ('t4', 'other', 1, NULL);",
            )
            .unwrap();
        let coverage = store
            .stats_window(&StatsWindow {
                since: Some(start),
                source: Some("codex".into()),
                project_prefix: None,
            })
            .unwrap()
            .coverage;
        assert_eq!(coverage.window_start, start);
        assert_eq!(coverage.window_end, end);
        assert_eq!(coverage.sessions_in_window, 3);
        assert_eq!(coverage.eligible_sessions, 3);
        assert_eq!(coverage.eligible_turns, 3);
        assert_eq!(coverage.unpriced_sessions, 3);
        assert_eq!(coverage.sessions_missing_turn_timestamps, 2);
        assert!(coverage.failed_sources.is_empty());
    }

    #[test]
    fn bad_updated_at_is_error_not_now() {
        let store = InsightsStore::open_in_memory().unwrap();
        store
            .upsert_session(&sample_session("bad", SessionSource::Codex, None))
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE sessions SET updated_at='garbage' WHERE id='bad'",
                [],
            )
            .unwrap();
        assert!(matches!(
            store.get_session("bad"),
            Err(StoreError::Parse(_))
        ));
    }

    #[test]
    fn empty_future_window_bounds_stay_in_order() {
        let store = InsightsStore::open_in_memory().unwrap();
        let since = Utc::now() + chrono::Duration::days(1);
        let c = store
            .stats_window(&StatsWindow {
                since: Some(since),
                ..Default::default()
            })
            .unwrap()
            .coverage;
        assert_eq!(c.window_start, since);
        assert!(c.window_end >= c.window_start);
    }

    #[test]
    fn empty_session_list_does_not_report_epoch_bounds() {
        let before = Utc::now();
        let c = crate::analytics::compute_analytics(&[]).coverage;
        assert!(c.window_start >= before);
        assert_eq!(c.window_start, c.window_end);
    }

    #[test]
    fn stats_window_reports_cost_basis_mix_for_priced_sessions() {
        let store = InsightsStore::open_in_memory().unwrap();
        let mut priced = sample_session("p", SessionSource::Codex, None);
        priced.cost_basis = CostBasis::Estimated;
        priced.cost = Some(Cost {
            total_usd: 1.0,
            input_usd: 0.0,
            output_usd: 0.0,
            cache_read_usd: 0.0,
            cache_write_usd: 0.0,
        });
        store.upsert_session(&priced).unwrap();
        store
            .upsert_session(&sample_session("u", SessionSource::Codex, None))
            .unwrap();
        let mix = store
            .stats_window(&StatsWindow::default())
            .unwrap()
            .coverage
            .cost_basis_mix;
        assert_eq!(mix.len(), 1);
        assert_eq!(mix["estimated"], 1);
        let from_list = crate::analytics::compute_analytics(&[priced]).coverage;
        assert_eq!(from_list.cost_basis_mix["estimated"], 1);
    }

    #[test]
    fn no_lower_bound_keeps_expanded_year_timestamps() {
        use chrono::TimeZone;
        let store = InsightsStore::open_in_memory().unwrap();
        let mut far = sample_session("far", SessionSource::Codex, None);
        far.updated_at = Utc.with_ymd_and_hms(10000, 1, 1, 0, 0, 0).unwrap();
        store.upsert_session(&far).unwrap();
        let rows = store.sessions_in_window(None, None, None, None).unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn stats_window_reads_one_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("i.db");
        let store = InsightsStore::open(&path).unwrap();
        let mut n = 0;
        for (src, proj, count) in [
            (SessionSource::Codex, "a", 3),
            (SessionSource::ClaudeCode, "a", 2),
            (SessionSource::Codex, "b", 4),
        ] {
            for _ in 0..count {
                let mut s = sample_session(&format!("s{n}"), src, None);
                s.project = proj.into();
                store.upsert_session(&s).unwrap();
                n += 1;
            }
        }
        let a = store.stats_window(&StatsWindow::default()).unwrap();
        assert_eq!(a.total_sessions, 9);
        assert_eq!(a.by_source.values().sum::<usize>(), a.total_sessions);
        assert_eq!(a.by_project.values().sum::<usize>(), a.total_sessions);

        let other = InsightsStore::open(&path).unwrap();
        other
            .upsert_session(&sample_session("late", SessionSource::Codex, None))
            .unwrap();
        assert_eq!(a.total_sessions, 9);
        let b = store.stats_window(&StatsWindow::default()).unwrap();
        assert_eq!(b.total_sessions, 10);
        assert_eq!(b.by_source.values().sum::<usize>(), 10);
    }
}
