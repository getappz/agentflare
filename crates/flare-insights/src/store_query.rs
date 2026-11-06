//! SQL-side query helpers for InsightsStore.
//!
//! `sessions.source` / `sessions.status` are stored via `serde_json::to_string`,
//! so filters must compare against the JSON-quoted form (e.g. `"codex"`).

use chrono::{DateTime, Utc};
use rusqlite::params_from_iter;
use rusqlite::types::Value;
use serde::Serialize;

use crate::analytics::{cache_metrics, Analytics, DailyBucket};
use crate::evidence::CoverageMeta;

use crate::model::{Session, SessionSource, TokenUsage};
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

/// `<select> WHERE 1=1 [AND ...]` with optional since/source/project-prefix filters.
fn sessions_select(
    select: &str,
    since: Option<DateTime<Utc>>,
    source: Option<&str>,
    project_prefix: Option<&str>,
) -> (String, Vec<Value>) {
    let mut sql = format!("{select} WHERE 1=1");
    let mut binds: Vec<Value> = Vec::new();
    if let Some(t) = since {
        sql.push_str(" AND updated_at >= ?");
        binds.push(Value::Text(t.to_rfc3339()));
    }

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
            sessions_select("SELECT * FROM sessions", None, source, project_prefix);
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

    /// Aggregate stats over a window with SQL GROUP BY (no row-count cap).
    pub fn stats_window(&self, w: &StatsWindow) -> Result<Analytics, StoreError> {
        let (filter, binds) = sessions_select(
            "SELECT id FROM sessions",
            w.since,
            w.source.as_deref(),
            w.project_prefix.as_deref(),
        );
        let ids = format!("session_id IN ({filter})");
        let conn = &self.conn;

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
        a.coverage.window_end = last.map(parse).transpose()?.unwrap_or_else(Utc::now);
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
}
