use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;

use super::observations::Observation;
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
    // The write may have changed what this project's recalls return
    // (including `duplicate`, which bumps duplicate_count/last_seen_at).
    if let Ok(mut cache) = recall_cache().lock() {
        cache.invalidate_project(input.project.as_deref());
    }
    // Best-effort semantic index; failure must never fail the remember.
    // Duplicates keep their existing vector — content is unchanged by definition.
    if status != "duplicate" {
        let text = format!("{}\n{}", input.title, input.content);
        match super::engine::embed_doc(&text) {
            Some(vec) => {
                let model = super::engine::model_name().unwrap_or_default();
                if let Err(e) = super::embeddings::upsert(&conn, id, &vec, &model) {
                    eprintln!("[memory] embedding upsert failed for obs {id}: {e}");
                    // Refresh failed — drop any stale vector so recall never ranks
                    // on outdated content; `missing` re-surfaces it for backfill.
                    let _ = super::embeddings::delete(&conn, id);
                }
            }
            // No embedder available. A prior vector (from when a model was present)
            // would now point at stale content on an update — remove it. Created
            // rows have no vector yet, so this is a no-op for them.
            None if status == "updated" => {
                let _ = super::embeddings::delete(&conn, id);
            }
            None => {}
        }
    }
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

/// Short-TTL in-process dedup cache for recall.
///
/// Session-start / prompt-submit paths re-issue identical recalls
/// back-to-back; without this every call re-runs FTS (+ embedding) and can
/// return duplicate rows across calls. Process-local only — no
/// cross-machine semantics. Only successful non-empty query results are
/// cached; `id=` lookups and query-less listings bypass it.
struct RecallCacheEntry {
    inserted: Instant,
    /// Lowercased project this entry belongs to (`""` when unscoped), so a
    /// `remember`/`curate` can invalidate exactly its own project's entries.
    project: String,
    /// Serialized response (`json!(results).to_string()`), ready to return.
    response: String,
}

struct RecallCache {
    entries: HashMap<String, RecallCacheEntry>,
    ttl: Duration,
    cap: usize,
}

const RECALL_CACHE_TTL_SECS: u64 = 45;
const RECALL_CACHE_CAP: usize = 128;

impl RecallCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            ttl: Duration::from_secs(RECALL_CACHE_TTL_SECS),
            cap: RECALL_CACHE_CAP,
        }
    }

    /// Cache key: normalized(query) + project + type + limit, using the same
    /// normalizer as `observations::hash_normalized` so queries differing
    /// only in case/whitespace share an entry.
    fn key(query: &str, project: Option<&str>, r#type: Option<&str>, limit: usize) -> String {
        format!(
            "{}|{}|{}|{limit}",
            observations::normalize_text(query),
            project.unwrap_or("").to_lowercase(),
            r#type.unwrap_or("").to_lowercase(),
        )
    }

    fn get(&mut self, key: &str) -> Option<String> {
        match self.entries.get(key) {
            Some(e) if e.inserted.elapsed() < self.ttl => Some(e.response.clone()),
            Some(_) => {
                self.entries.remove(key);
                None
            }
            None => None,
        }
    }

    fn insert(&mut self, key: String, project: String, response: String) {
        self.entries.retain(|_, e| e.inserted.elapsed() < self.ttl);
        if self.entries.len() >= self.cap
            && let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.inserted)
                .map(|(k, _)| k.to_string())
        {
            self.entries.remove(&oldest);
        }
        self.entries.insert(
            key,
            RecallCacheEntry {
                inserted: Instant::now(),
                project,
                response,
            },
        );
    }

    fn invalidate_project(&mut self, project: Option<&str>) {
        let norm = project.unwrap_or("").to_lowercase();
        self.entries.retain(|_, e| e.project != norm);
    }
}

fn recall_cache() -> &'static Mutex<RecallCache> {
    static CACHE: OnceLock<Mutex<RecallCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(RecallCache::new()))
}

/// Best-effort invalidation of one observation's project after a mutation.
/// Failures (poisoned lock, missing row) are ignored: the TTL bounds any
/// staleness, so invalidation must never fail the write itself.
fn invalidate_project_of(conn: &rusqlite::Connection, id: i64) {
    let project = observations::get(conn, id)
        .ok()
        .flatten()
        .and_then(|o| o.project);
    if let Ok(mut cache) = recall_cache().lock() {
        cache.invalidate_project(project.as_deref());
    }
}

#[cfg(test)]
fn clear_recall_cache() {
    if let Ok(mut cache) = recall_cache().lock() {
        cache.entries.clear();
    }
}

/// Serializes the recall-cache tests: the cache is process-global and test
/// threads run in parallel, so one test's `clear_recall_cache` could
/// otherwise land between another test's two fetches and flake its counts.
#[cfg(test)]
fn recall_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub fn handle_recall(input: RecallInput) -> Result<String, String> {
    let conn = open_db()?;
    recall_with_conn(&conn, input)
}

/// Core of `handle_recall`, taking an explicit connection so tests can run
/// against an isolated in-memory database instead of the real brain.db.
fn recall_with_conn(conn: &rusqlite::Connection, input: RecallInput) -> Result<String, String> {
    recall_with_fetch(conn, input, &|conn, q, project, r#type, limit| {
        search::search_hybrid(conn, q, project, r#type, limit, super::engine::embed_query)
    })
}

/// `recall_with_conn` with the underlying search injected, so tests can
/// count invocations and prove repeated identical recalls do one search.
/// Production passes `search::search_hybrid` (+ the real embedder).
type RecallFetch<'a> = &'a dyn Fn(
    &rusqlite::Connection,
    &str,
    Option<&str>,
    Option<&str>,
    usize,
) -> Result<Vec<Observation>, rusqlite::Error>;
fn recall_with_fetch(
    conn: &rusqlite::Connection,
    input: RecallInput,
    fetch: RecallFetch<'_>,
) -> Result<String, String> {
    if let Some(id) = input.id {
        let obs = observations::get(conn, id).map_err(|e| format!("lookup failed: {e}"))?;
        return Ok(json!(obs).to_string());
    }
    let limit = input.limit.unwrap_or(10).min(50);
    if let Some(ref q) = input.query.filter(|q| !q.trim().is_empty()) {
        let key = RecallCache::key(q, input.project.as_deref(), input.r#type.as_deref(), limit);
        if let Ok(mut cache) = recall_cache().lock()
            && let Some(hit) = cache.get(&key)
        {
            return Ok(hit);
        }
        let mut results = fetch(
            conn,
            q,
            input.project.as_deref(),
            input.r#type.as_deref(),
            limit,
        )
        .map_err(|e| format!("search failed: {e}"))?;
        // Never return the same observation twice from one recall
        // (FTS/vector merge paths can overlap).
        let mut seen = HashSet::new();
        results.retain(|o| seen.insert(o.id));
        let response = json!(results).to_string();
        if !results.is_empty()
            && let Ok(mut cache) = recall_cache().lock()
        {
            cache.insert(
                key,
                input.project.as_deref().unwrap_or("").to_lowercase(),
                response.clone(),
            );
        }
        return Ok(response);
    }
    let results = observations::list_recent(
        conn,
        input.project.as_deref(),
        input.r#type.as_deref(),
        limit,
    )
    .map_err(|e| format!("list failed: {e}"))?;
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
        return Ok(
            serde_json::json!({"lines": [], "kept": 0, "total": 0, "query": query}).to_string(),
        );
    }

    let scored =
        crate::compact::score_lines(&entries, &query).map_err(|e| format!("score lines: {e}"))?;

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
            if ok {
                invalidate_project_of(&conn, input.id);
            }
            Ok(json!({"status": if ok { "updated" } else { "not_found" }}).to_string())
        }
        "delete" => {
            let ok =
                observations::soft_delete(&conn, input.id).map_err(|e| format!("delete: {e}"))?;
            if ok {
                invalidate_project_of(&conn, input.id);
            }
            Ok(json!({"status": if ok { "deleted" } else { "not_found" }}).to_string())
        }
        "pin" => {
            let ok = observations::pin(&conn, input.id, true).map_err(|e| format!("pin: {e}"))?;
            if ok {
                invalidate_project_of(&conn, input.id);
            }
            Ok(json!({"status": if ok { "pinned" } else { "not_found" }}).to_string())
        }
        "unpin" => {
            let ok =
                observations::pin(&conn, input.id, false).map_err(|e| format!("unpin: {e}"))?;
            if ok {
                invalidate_project_of(&conn, input.id);
            }
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
    use std::cell::Cell;

    fn new_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        super::super::schema::migrate(&mut conn).unwrap();
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

    // Item 217: repeated identical recalls within the TTL must run a single
    // underlying search; the second call is served from the dedup cache.
    // (Cache keys use per-test-unique tokens because the cache is
    // process-global and tests run in parallel threads.)
    #[test]
    fn recall_caches_identical_query_within_ttl() {
        let _cache_guard = recall_test_lock().lock().unwrap();
        clear_recall_cache();
        let conn = new_db();
        observations::save(
            &conn,
            None,
            "decision",
            "dedup cache marker alpha",
            "content about the dedup cache marker alpha",
            None,
            Some("proj-cache"),
            None,
            None,
        )
        .unwrap();
        let calls = Cell::new(0usize);
        let fetch = |conn: &rusqlite::Connection,
                     q: &str,
                     project: Option<&str>,
                     r#type: Option<&str>,
                     limit: usize| {
            calls.set(calls.get() + 1);
            search::search_hybrid(conn, q, project, r#type, limit, |_| None)
        };
        let mk_input = || RecallInput {
            query: Some("dedup cache marker alpha".to_string()),
            id: None,
            r#type: None,
            project: Some("proj-cache".to_string()),
            limit: Some(10),
        };
        let first = recall_with_fetch(&conn, mk_input(), &fetch).unwrap();
        let second = recall_with_fetch(&conn, mk_input(), &fetch).unwrap();
        assert_eq!(first, second);
        assert_eq!(calls.get(), 1, "second identical recall must hit the cache");
        let v: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert!(!v.as_array().unwrap().is_empty());
    }

    // Item 217: project/type/limit are part of the cache key (each misses),
    // while case/whitespace-only differences hit the same entry.
    #[test]
    fn recall_cache_key_covers_project_type_limit_not_case() {
        let _cache_guard = recall_test_lock().lock().unwrap();
        clear_recall_cache();
        let conn = new_db();
        for (typ, project) in [
            ("decision", "proj-kb-a"),
            ("bugfix", "proj-kb-a"),
            ("decision", "proj-kb-b"),
        ] {
            observations::save(
                &conn,
                None,
                typ,
                "key coverage marker beta",
                "content about the key coverage marker beta",
                None,
                Some(project),
                None,
                None,
            )
            .unwrap();
        }
        let calls = Cell::new(0usize);
        let fetch = |conn: &rusqlite::Connection,
                     q: &str,
                     project: Option<&str>,
                     r#type: Option<&str>,
                     limit: usize| {
            calls.set(calls.get() + 1);
            search::search_hybrid(conn, q, project, r#type, limit, |_| None)
        };
        let mk = |project: &str, typ: Option<&str>, limit: usize, query: &str| RecallInput {
            query: Some(query.to_string()),
            id: None,
            r#type: typ.map(|t| t.to_string()),
            project: Some(project.to_string()),
            limit: Some(limit),
        };
        recall_with_fetch(
            &conn,
            mk("proj-kb-a", None, 10, "key coverage marker beta"),
            &fetch,
        )
        .unwrap();
        assert_eq!(calls.get(), 1);
        // Identical again: hit.
        recall_with_fetch(
            &conn,
            mk("proj-kb-a", None, 10, "key coverage marker beta"),
            &fetch,
        )
        .unwrap();
        assert_eq!(calls.get(), 1);
        // Case + whitespace noise: same normalized key, still a hit.
        recall_with_fetch(
            &conn,
            mk("proj-kb-a", None, 10, "  KEY   Coverage Marker Beta "),
            &fetch,
        )
        .unwrap();
        assert_eq!(calls.get(), 1);
        // Different project / type / limit: each misses exactly once.
        recall_with_fetch(
            &conn,
            mk("proj-kb-b", None, 10, "key coverage marker beta"),
            &fetch,
        )
        .unwrap();
        assert_eq!(calls.get(), 2);
        recall_with_fetch(
            &conn,
            mk("proj-kb-a", Some("bugfix"), 10, "key coverage marker beta"),
            &fetch,
        )
        .unwrap();
        assert_eq!(calls.get(), 3);
        recall_with_fetch(
            &conn,
            mk("proj-kb-a", None, 5, "key coverage marker beta"),
            &fetch,
        )
        .unwrap();
        assert_eq!(calls.get(), 4);
    }

    // Item 217: empty results are never cached, and invalidating a project
    // forces the next identical recall to re-search.
    #[test]
    fn recall_skips_cache_for_empty_results_and_honors_invalidation() {
        let _cache_guard = recall_test_lock().lock().unwrap();
        clear_recall_cache();
        let conn = new_db();
        observations::save(
            &conn,
            None,
            "decision",
            "invalidation marker gamma",
            "content about the invalidation marker gamma",
            None,
            Some("proj-inv"),
            None,
            None,
        )
        .unwrap();
        let calls = Cell::new(0usize);
        let fetch = |conn: &rusqlite::Connection,
                     q: &str,
                     project: Option<&str>,
                     r#type: Option<&str>,
                     limit: usize| {
            calls.set(calls.get() + 1);
            search::search_hybrid(conn, q, project, r#type, limit, |_| None)
        };
        let mk = |query: &str| RecallInput {
            query: Some(query.to_string()),
            id: None,
            r#type: None,
            project: Some("proj-inv".to_string()),
            limit: Some(10),
        };
        // Empty: two calls, two searches — never cached.
        let empty = recall_with_fetch(&conn, mk("zzz-no-such-token-gamma"), &fetch).unwrap();
        assert_eq!(empty, "[]");
        recall_with_fetch(&conn, mk("zzz-no-such-token-gamma"), &fetch).unwrap();
        assert_eq!(calls.get(), 2);
        // Non-empty: second call hits the cache...
        recall_with_fetch(&conn, mk("invalidation marker gamma"), &fetch).unwrap();
        recall_with_fetch(&conn, mk("invalidation marker gamma"), &fetch).unwrap();
        assert_eq!(calls.get(), 3);
        // ...until its project is invalidated.
        if let Ok(mut cache) = recall_cache().lock() {
            cache.invalidate_project(Some("proj-inv"));
        }
        recall_with_fetch(&conn, mk("invalidation marker gamma"), &fetch).unwrap();
        assert_eq!(calls.get(), 4);
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

    #[test]
    fn handle_compact_empty_lines_response_includes_query() {
        let input = CompactInput {
            lines: String::new(),
            query: Some("some query".to_string()),
            compression_ratio: None,
            preserve_recent: None,
            scorer: None,
        };
        let out = handle_compact(input).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["query"], "some query",
            "empty-input response must include query like the full response does, got {v}"
        );
        assert_eq!(v["kept"], 0);
        assert_eq!(v["total"], 0);
    }
}
