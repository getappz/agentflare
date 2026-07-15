use serde::Deserialize;
use serde_json::json;

use super::store;
use super::{observations, relations, search, sessions, summaries};

fn open_db() -> Result<rusqlite::Connection, String> {
    store::open().map_err(|e| format!("cannot open brain.db: {e}"))
}

#[derive(Debug, Deserialize)]
pub struct RememberInput {
    pub title: String,
    pub content: String,
    pub r#type: String,
    pub session_id: Option<String>,
    pub project: Option<String>,
    pub topic_key: Option<String>,
    pub scope: Option<String>,
}

pub fn handle_remember(input: RememberInput) -> Result<String, String> {
    if input.title.trim().is_empty() || input.content.trim().is_empty() {
        return Err("title and content are required".into());
    }
    let conn = open_db()?;
    let outcome = observations::save(
        &conn,
        input.session_id.as_deref(),
        &input.r#type,
        &input.title,
        &input.content,
        None,
        input.project.as_deref(),
        input.scope.as_deref(),
        input.topic_key.as_deref(),
    )
    .map_err(|e| format!("save failed: {e}"))?;
    let (status, id) = match outcome {
        observations::SaveOutcome::Created(id) => ("created", id),
        observations::SaveOutcome::Updated(id) => ("updated", id),
        observations::SaveOutcome::Duplicate(id) => ("duplicate", id),
    };
    Ok(json!({"status": status, "id": id}).to_string())
}

#[derive(Debug, Deserialize)]
pub struct RecallInput {
    pub query: Option<String>,
    pub id: Option<i64>,
    pub r#type: Option<String>,
    pub project: Option<String>,
    pub limit: Option<usize>,
}

pub fn handle_recall(input: RecallInput) -> Result<String, String> {
    let conn = open_db()?;
    recall_with_conn(&conn, input)
}

/// Core of `handle_recall`, taking an explicit connection so tests can run
/// against an isolated in-memory database instead of the real brain.db.
fn recall_with_conn(conn: &rusqlite::Connection, input: RecallInput) -> Result<String, String> {
    if let Some(id) = input.id {
        let obs = observations::get(conn, id).map_err(|e| format!("lookup failed: {e}"))?;
        return Ok(json!(obs).to_string());
    }
    let limit = input.limit.unwrap_or(10).min(50);
    let results = if let Some(ref q) = input.query.filter(|q| !q.trim().is_empty()) {
        search::search(
            conn,
            q,
            input.project.as_deref(),
            input.r#type.as_deref(),
            limit,
        )
        .map_err(|e| format!("search failed: {e}"))?
    } else {
        observations::list_recent(
            conn,
            input.project.as_deref(),
            input.r#type.as_deref(),
            limit,
        )
        .map_err(|e| format!("list failed: {e}"))?
    };
    Ok(json!(results).to_string())
}

#[derive(Debug, Deserialize)]
pub struct ContextInput {
    pub session_id: Option<String>,
    pub project: Option<String>,
}

pub fn handle_context(input: ContextInput) -> Result<String, String> {
    let conn = open_db()?;
    let session = match input.session_id.as_deref() {
        Some(id) if !id.is_empty() => {
            sessions::get(&conn, id).map_err(|e| format!("session lookup: {e}"))?
        }
        _ => None,
    };
    let recent_sessions = sessions::list_recent(&conn, input.project.as_deref(), 5)
        .map_err(|e| format!("recent sessions: {e}"))?;
    let recent_obs = observations::list_recent(&conn, input.project.as_deref(), None, 10)
        .map_err(|e| format!("recent observations: {e}"))?;
    let recent_summaries = summaries::list_recent(&conn, input.project.as_deref(), 5)
        .map_err(|e| format!("recent summaries: {e}"))?;
    Ok(json!({
        "session": session,
        "recent_sessions": recent_sessions,
        "recent_observations": recent_obs,
        "recent_summaries": recent_summaries,
    })
    .to_string())
}

#[derive(Debug, Deserialize)]
pub struct HandoffInput {
    pub session_id: String,
    pub summary: String,
    pub findings: Option<Vec<serde_json::Value>>,
    pub decisions: Option<Vec<serde_json::Value>>,
    pub files_touched: Option<Vec<serde_json::Value>>,
    pub evidence: Option<Vec<serde_json::Value>>,
}

pub fn handle_handoff(input: HandoffInput) -> Result<String, String> {
    let conn = open_db()?;
    handoff_with_conn(&conn, input)
}

/// Core of `handle_handoff`, taking an explicit connection so tests can run
/// against an isolated in-memory database instead of the real brain.db.
fn handoff_with_conn(conn: &rusqlite::Connection, input: HandoffInput) -> Result<String, String> {
    if input.session_id.trim().is_empty() || input.summary.trim().is_empty() {
        return Err("session_id and summary are required".into());
    }
    // `unchecked_transaction` (not `Connection::transaction`, which needs
    // `&mut Connection`) so enrich+close+append commit atomically: a failure
    // partway through used to leave the session closed without its summary,
    // with no way to retry since it was already closed.
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("begin transaction: {e}"))?;

    // `sessions::create` was never called from any CLI subcommand or MCP
    // tool, so the sessions table was always empty in practice and this
    // lookup would unconditionally fail with "session not found" for every
    // session_id. Auto-create instead, so handoff works standalone.
    let session =
        match sessions::get(&tx, &input.session_id).map_err(|e| format!("session lookup: {e}"))? {
            Some(session) => session,
            None => sessions::create(&tx, &input.session_id, None, None)
                .map_err(|e| format!("create session: {e}"))?,
        };

    let project = session.project.clone().unwrap_or_default();
    let findings = input
        .findings
        .as_ref()
        .map(|v| serde_json::to_string(&v).unwrap_or_default());
    let decisions = input
        .decisions
        .as_ref()
        .map(|v| serde_json::to_string(&v).unwrap_or_default());
    let files = input
        .files_touched
        .as_ref()
        .map(|v| serde_json::to_string(&v).unwrap_or_default());
    let evidence = input
        .evidence
        .as_ref()
        .map(|v| serde_json::to_string(&v).unwrap_or_default());

    let snapshot = json!({
        "session_id": input.session_id,
        "project": session.project,
        "summary": input.summary,
        "findings_count": input.findings.as_ref().map(|v| v.len()).unwrap_or(0),
        "decisions_count": input.decisions.as_ref().map(|v| v.len()).unwrap_or(0),
        "files_touched_count": input.files_touched.as_ref().map(|v| v.len()).unwrap_or(0),
    })
    .to_string();

    // Single write covering findings/decisions/files/evidence AND the
    // snapshot — the snapshot never depended on the first write's result,
    // so the previous two-call version was a redundant extra round trip.
    sessions::update_enriched(
        &tx,
        &input.session_id,
        None,
        findings.as_deref(),
        decisions.as_deref(),
        files.as_deref(),
        evidence.as_deref(),
        None,
        Some(&snapshot),
    )
    .map_err(|e| format!("update enriched: {e}"))?;

    sessions::close(&tx, &input.session_id, &input.summary)
        .map_err(|e| format!("close session: {e}"))?;

    summaries::append(&tx, &project, Some(&input.session_id), &input.summary)
        .map_err(|e| format!("append summary: {e}"))?;

    tx.commit().map_err(|e| format!("commit handoff: {e}"))?;

    Ok(json!({
        "status": "closed",
        "session_id": input.session_id,
        "project": project,
        "snapshot": snapshot,
    })
    .to_string())
}

#[derive(Debug, Deserialize)]
pub struct RelateInput {
    pub source_id: i64,
    pub target_id: i64,
    pub relation: String,
    pub reason: Option<String>,
    pub confidence: Option<f64>,
}

pub fn handle_relate(input: RelateInput) -> Result<String, String> {
    if input.source_id == input.target_id {
        return Err("source_id and target_id must differ".into());
    }
    let conn = open_db()?;
    let id = relations::create(
        &conn,
        input.source_id,
        input.target_id,
        &input.relation,
        None,
        input.reason.as_deref(),
        None,
        input.confidence,
    )
    .map_err(|e| format!("create relation: {e}"))?;
    Ok(json!({"status": "created", "id": id}).to_string())
}

#[derive(Debug, Deserialize)]
pub struct CompactInput {
    pub lines: String,
    pub query: Option<String>,
    pub compression_ratio: Option<f64>,
    pub preserve_recent: Option<usize>,
    pub scorer: Option<String>,
}

pub fn handle_compact(input: CompactInput) -> Result<String, String> {
    if input.query.as_deref().is_none_or(|q| q.trim().is_empty()) {
        return Err("query is required".into());
    }
    if let Some(scorer) = input.scorer.as_deref()
        && scorer != "fts5"
    {
        return Err(format!(
            "unsupported scorer '{scorer}' -- only 'fts5' is implemented"
        ));
    }
    let query = input.query.unwrap();

    let entries: Vec<crate::compact::LineEntry> = input
        .lines
        .lines()
        .enumerate()
        .map(|(i, text)| crate::compact::LineEntry {
            index: i,
            text: text.to_string(),
        })
        .collect();

    if entries.is_empty() {
        return Ok(serde_json::json!({"lines": [], "kept": 0, "total": 0}).to_string());
    }

    let scored = crate::compact::score_lines(&entries, &query);

    let target = input.compression_ratio.unwrap_or(0.5).clamp(0.0, 1.0);
    let preserve = input.preserve_recent.unwrap_or(3);

    // Keep top scored lines up to target ratio, but protect recent ones.
    let keep_count = (entries.len() as f64 * target).ceil() as usize;
    let mut keep: Vec<bool> = vec![false; entries.len()];

    // Mark most recent N for unconditional keep.
    let recent_start = entries.len().saturating_sub(preserve);
    for k in &mut keep[recent_start..] {
        *k = true;
    }

    // Fill remaining keep quota with highest-scored (lowest BM25 score)
    // lines first, in the relevance order `score_lines` already returned --
    // do NOT re-sort by index, that would silently fall back to earliest-
    // in-transcript instead of most-relevant whenever there are more
    // matches than room to keep them.
    let by_relevance: Vec<usize> = scored.iter().map(|s| s.index).collect();
    let mut filled: usize = keep.iter().filter(|&&k| k).count();
    for idx in by_relevance {
        if filled >= keep_count {
            break;
        }
        if !keep[idx] {
            keep[idx] = true;
            filled += 1;
        }
    }

    let output: Vec<serde_json::Value> = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let score = scored.iter().find(|s| s.index == i).map(|s| s.score);
            serde_json::json!({
                "index": entry.index,
                "text": entry.text,
                "score": score,
                "keep": keep[i],
            })
        })
        .collect();

    Ok(serde_json::json!({
        "lines": output,
        "kept": keep.iter().filter(|&&k| k).count(),
        "total": entries.len(),
        "query": query,
    })
    .to_string())
}

#[derive(Debug, Deserialize)]
pub struct CurateInput {
    pub action: String,
    pub id: i64,
    pub title: Option<String>,
    pub content: Option<String>,
    pub r#type: Option<String>,
    pub pinned: Option<bool>,
}

pub fn handle_curate(input: CurateInput) -> Result<String, String> {
    let conn = open_db()?;
    match input.action.as_str() {
        "update" => {
            let ok = observations::update(
                &conn,
                input.id,
                input.title.as_deref(),
                input.content.as_deref(),
                input.r#type.as_deref(),
                input.pinned,
            )
            .map_err(|e| format!("update: {e}"))?;
            Ok(json!({"status": if ok { "updated" } else { "not_found" }}).to_string())
        }
        "delete" => {
            let ok =
                observations::soft_delete(&conn, input.id).map_err(|e| format!("delete: {e}"))?;
            Ok(json!({"status": if ok { "deleted" } else { "not_found" }}).to_string())
        }
        "pin" => {
            let ok = observations::pin(&conn, input.id, true).map_err(|e| format!("pin: {e}"))?;
            Ok(json!({"status": if ok { "pinned" } else { "not_found" }}).to_string())
        }
        "unpin" => {
            let ok =
                observations::pin(&conn, input.id, false).map_err(|e| format!("unpin: {e}"))?;
            Ok(json!({"status": if ok { "unpinned" } else { "not_found" }}).to_string())
        }
        other => Err(format!(
            "unknown action '{other}'; use update, delete, pin, or unpin"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn new_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        super::super::schema::migrate(&conn).unwrap();
        conn
    }

    // Regression test for item 10: sessions::create was never called from
    // any CLI subcommand or MCP tool, so `sessions::get` always returned
    // None and handoff unconditionally failed with "session not found".
    // handle_handoff must now auto-create the session and succeed.
    #[test]
    fn handoff_with_never_created_session_succeeds() {
        let conn = new_db();
        let input = HandoffInput {
            session_id: "never-seen-session".to_string(),
            summary: "closed out the work".to_string(),
            findings: None,
            decisions: None,
            files_touched: None,
            evidence: None,
        };
        let out = handoff_with_conn(&conn, input).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["status"], "closed");
        assert_eq!(v["session_id"], "never-seen-session");

        let session = sessions::get(&conn, "never-seen-session").unwrap().unwrap();
        assert_eq!(session.status, "closed");
        assert_eq!(session.summary.as_deref(), Some("closed out the work"));
    }

    // Regression test for item 8: the snapshot write must land even though
    // it's now folded into the same update_enriched call as the other
    // fields, and the whole sequence must actually commit.
    #[test]
    fn handoff_persists_enriched_fields_and_snapshot() {
        let conn = new_db();
        let input = HandoffInput {
            session_id: "sess-enrich".to_string(),
            summary: "did the thing".to_string(),
            findings: Some(vec![
                serde_json::json!({"file": "a.rs", "summary": "found x"}),
            ]),
            decisions: None,
            files_touched: None,
            evidence: None,
        };
        handoff_with_conn(&conn, input).unwrap();

        let session = sessions::get(&conn, "sess-enrich").unwrap().unwrap();
        assert!(session.findings.contains("found x"));
        assert!(session.compaction_snapshot.is_some());
        assert!(
            session
                .compaction_snapshot
                .unwrap()
                .contains("did the thing")
        );
    }

    // Regression test for item 7: the type filter used to be applied via
    // post-fetch `.filter(...)` on already-limited results, so type-matching
    // rows past the naive limit were silently dropped. It must now be
    // applied in SQL, before LIMIT.
    #[test]
    fn recall_type_filter_finds_rows_past_naive_limit() {
        let conn = new_db();
        // The decision row is the OLDEST row; several newer bugfix rows
        // follow it. list_recent orders by created_at DESC, so a naive
        // "fetch top-N then filter by type" with a small limit would only
        // ever see the newer bugfix rows and never reach the decision row.
        observations::save(
            &conn,
            None,
            "decision",
            "the one decision",
            "the only decision here",
            None,
            Some("proj-a"),
            None,
            None,
        )
        .unwrap();
        for i in 0..5 {
            observations::save(
                &conn,
                None,
                "bugfix",
                &format!("bug {i}"),
                "irrelevant filler content",
                None,
                Some("proj-a"),
                None,
                None,
            )
            .unwrap();
        }

        let input = RecallInput {
            query: None,
            id: None,
            r#type: Some("decision".to_string()),
            project: Some("proj-a".to_string()),
            limit: Some(2),
        };
        let out = recall_with_conn(&conn, input).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], "decision");
    }
    #[test]
    fn handle_compact_fills_quota_by_relevance_not_transcript_order() {
        let lines = [
            "mentions cache just once here",
            "filler filler filler one",
            "filler filler filler two",
            "filler filler filler three",
            "filler filler filler four",
            "cache cache cache invalidation logic here",
            "filler filler filler five",
        ]
        .join(
            "
",
        );
        let input = CompactInput {
            lines,
            query: Some("cache".to_string()),
            compression_ratio: Some(0.1),
            preserve_recent: Some(0),
            scorer: None,
        };
        let out = handle_compact(input).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["kept"], 1, "expected exactly one line kept, got {v}");
        let kept_indices: Vec<i64> = v["lines"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|l| l["keep"] == true)
            .map(|l| l["index"].as_i64().unwrap())
            .collect();
        assert_eq!(
            kept_indices,
            vec![5],
            "expected the more-relevant line (index 5, 3 mentions) to be kept over the earlier weaker match (index 0, 1 mention)"
        );
    }

    #[test]
    fn handle_compact_rejects_unsupported_scorer() {
        let input = CompactInput {
            lines: "some line".to_string(),
            query: Some("some".to_string()),
            compression_ratio: None,
            preserve_recent: None,
            scorer: Some("keyword".to_string()),
        };
        let err = handle_compact(input).unwrap_err();
        assert!(err.contains("keyword"), "{err}");
    }

    #[test]
    fn handle_compact_accepts_fts5_scorer() {
        let input = CompactInput {
            lines: "some line".to_string(),
            query: Some("some".to_string()),
            compression_ratio: None,
            preserve_recent: None,
            scorer: Some("fts5".to_string()),
        };
        assert!(handle_compact(input).is_ok());
    }
}
