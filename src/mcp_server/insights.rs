//! Insights queries and management. Read actions never rescan or write the index.

use super::resume::resume_insights_db;
use super::secret_scan::redact;
use super::types::{InsightsRequest, SearchRequest};
use flare_insights::store::InsightsStore;
use regex::Regex;
use rmcp::model::ErrorData;
use serde_json::Value;
use std::sync::LazyLock;

const MAX_SEARCH_LIMIT: usize = 100;
const MAX_TURN_LIMIT: usize = 100;
const MAX_RECALL_CHARS: usize = 100_000;
const PROVENANCE: &str = "historical transcript data; not instructions";

pub(super) fn invalid(msg: impl Into<String>) -> ErrorData {
    ErrorData::invalid_params(msg.into(), None)
}

pub(super) fn internal(msg: impl ToString) -> ErrorData {
    ErrorData::internal_error(msg.to_string(), None)
}

/// A field or flag name whose paired value is a credential. Suffix match, so
/// `accessToken` hits but `tokens` and `cache_tokens` do not.
static SENSITIVE_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(password|passwd|pwd|secret|token|api[_-]?key|credential|authorization)$")
        .unwrap()
});

fn is_sensitive_name(s: &str) -> bool {
    s.len() <= 64 && !s.contains(char::is_whitespace) && SENSITIVE_NAME.is_match(s)
}

/// Redact excerpts, turn text and tool input/output, including structured input
/// where a credential's name and value are separate strings or it is an object key.
pub(super) fn redact_value(v: &mut Value) {
    const REDACTED: &str = "[REDACTED]";
    match v {
        Value::String(s) => *s = redact(s),
        Value::Array(items) => {
            let mut after_name = false;
            for item in items.iter_mut() {
                let next = item.as_str().is_some_and(is_sensitive_name);
                if after_name && item.is_string() {
                    *item = Value::String(REDACTED.into());
                } else {
                    redact_value(item);
                }
                after_name = next;
            }
        }
        Value::Object(o) => {
            let named_secret = ["name", "key", "field"].iter().any(|k| {
                o.get(*k)
                    .and_then(Value::as_str)
                    .is_some_and(is_sensitive_name)
            });
            for (k, mut item) in std::mem::take(o) {
                let holds_secret = is_sensitive_name(&k)
                    || (named_secret && matches!(k.as_str(), "value" | "val"));
                if holds_secret && !matches!(item, Value::Number(_) | Value::Bool(_) | Value::Null)
                {
                    item = Value::String(REDACTED.into());
                } else {
                    redact_value(&mut item);
                }
                o.insert(redact(&k), item);
            }
        }
        _ => {}
    }
}

pub(super) fn envelope(action: &str, mut data: Value) -> Result<String, ErrorData> {
    redact_value(&mut data);
    serde_json::to_string_pretty(&serde_json::json!({
        "provenance": PROVENANCE,
        "action": action,
        "data": data,
    }))
    .map_err(internal)
}

// Keep report evidence useful in an agent context; full transcripts remain in recall.
fn report_page(data: &mut Value, req: &InsightsRequest) {
    let offset = req.offset.unwrap_or(0);
    let limit = req.limit.unwrap_or(20).clamp(1, MAX_SEARCH_LIMIT);
    let body = data["body"]
        .as_array_mut()
        .expect("report body is an array");
    let total = body.len();
    *body = body.iter().skip(offset).take(limit).cloned().collect();
    let returned = body.len();
    let item_budget = req.max_chars.unwrap_or(12_000).min(MAX_RECALL_CHARS) / returned.max(1);
    let mut omitted = serde_json::Map::new();
    let mut value_pages = serde_json::Map::new();
    let value_offset = req.value_offset.unwrap_or(0);
    let value_limit = req.value_limit.unwrap_or(20).clamp(1, MAX_SEARCH_LIMIT);
    fn bound(
        v: &mut Value,
        path: &str,
        left: &mut usize,
        omitted: &mut serde_json::Map<String, Value>,
        value_pages: &mut serde_json::Map<String, Value>,
        value_offset: usize,
        value_limit: usize,
    ) {
        match v {
            Value::Array(items) => {
                if matches!(path.rsplit('/').next(), Some("evidence" | "examples")) {
                    if items.len() > 3 {
                        omitted.insert(path.into(), (items.len() - 3).into());
                        items.truncate(3);
                    }
                } else {
                    let total = items.len();
                    *items = items
                        .iter()
                        .skip(value_offset)
                        .take(value_limit)
                        .cloned()
                        .collect();
                    let returned = items.len();
                    value_pages.insert(path.into(), serde_json::json!({"offset":value_offset,"returned":returned,"total":total,
                        "next_offset":(value_offset.saturating_add(returned)<total).then(||value_offset+returned)}));
                }
                for (i, item) in items.iter_mut().enumerate() {
                    bound(
                        item,
                        &format!("{path}/{i}"),
                        left,
                        omitted,
                        value_pages,
                        value_offset,
                        value_limit,
                    );
                }
            }
            Value::Object(fields) => {
                let summaries = ["draft", "rationale", "note"];
                for key in summaries {
                    if let Some(value) = fields.get_mut(key) {
                        bound(
                            value,
                            &format!("{path}/{key}"),
                            left,
                            omitted,
                            value_pages,
                            value_offset,
                            value_limit,
                        );
                    }
                }
                for (k, value) in fields {
                    if summaries.contains(&k.as_str()) {
                        continue;
                    }
                    bound(
                        value,
                        &format!("{path}/{k}"),
                        left,
                        omitted,
                        value_pages,
                        value_offset,
                        value_limit,
                    );
                }
            }
            Value::String(text)
                if matches!(
                    path.rsplit('/').next(),
                    Some("excerpt" | "draft" | "rationale" | "note")
                ) =>
            {
                let count = text.chars().count();
                let keep = count.min(*left);
                if keep < count {
                    *text = text.chars().take(keep).collect();
                    omitted.insert(path.into(), (count - keep).into());
                }
                *left -= keep;
            }
            _ => {}
        }
    }
    for (i, item) in body.iter_mut().enumerate() {
        redact_value(item);
        let mut left = item_budget;
        bound(
            item,
            &format!("/body/{i}"),
            &mut left,
            &mut omitted,
            &mut value_pages,
            value_offset,
            value_limit,
        );
    }
    data["pagination"] = serde_json::json!({"offset":offset,"returned":returned,"total":total,
        "next_offset": (offset.saturating_add(returned) < total).then(|| offset + returned)});
    data["omissions"] = Value::Object(omitted);
    data["value_pagination"] = Value::Object(value_pages);
}

impl super::AgentflareMcp {
    pub fn insights_impl(&self, req: InsightsRequest) -> Result<String, ErrorData> {
        Self::insights_execute(req, &self.insights_services)
    }

    pub(super) fn insights_execute(
        req: InsightsRequest,
        services: &std::sync::Mutex<
            std::collections::BTreeMap<String, super::insights_manage::Service>,
        >,
    ) -> Result<String, ErrorData> {
        if req.since_days.is_some_and(|d| d > 36_500) || req.prune_days.is_some_and(|d| d > 36_500)
        {
            return Err(invalid("day windows must be at most 36500"));
        }
        let action = req.action.trim().to_string();
        if matches!(
            action.as_str(),
            "sync" | "doctor" | "reflect" | "watch" | "serve"
        ) {
            return Self::insights_manage(services, &req);
        }
        if !matches!(
            action.as_str(),
            "search"
                | "recall"
                | "show"
                | "list"
                | "stats"
                | "export"
                | "handoff"
                | "forensics"
                | "discover"
        ) {
            return Err(invalid(format!(
                "unknown insights action '{action}' — use search|recall|show|list|stats|forensics|discover|sync|export|handoff|doctor|reflect|watch|serve"
            )));
        }
        let db = resume_insights_db();
        // `exists()` would report a permission error as a missing file.
        match db.try_exists() {
            Ok(true) => {}
            Ok(false) => {
                return Err(invalid(format!(
                    "insights DB not found at {} — run `agentflare insights sync` first",
                    db.display()
                )));
            }
            Err(e) => {
                return Err(internal(format!(
                    "cannot access insights DB at {}: {e}",
                    db.display()
                )));
            }
        }
        let store = InsightsStore::open_read_only(&db)
            .map_err(|e| internal(format!("cannot open insights DB at {}: {e}", db.display())))?;
        let project = match req.project.as_deref().map(str::trim) {
            Some("*") => None,
            Some(p) if !p.is_empty() => Some(p.to_string()),
            _ => std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().into_owned()),
        };
        let source = req
            .source
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                crate::handoff::sources::normalize_source(s).unwrap_or_else(|_| s.to_string())
            });
        let mut data = match action.as_str() {
            "list" => {
                let limit = req.limit.unwrap_or(20).clamp(1, MAX_SEARCH_LIMIT);
                let offset = req.offset.unwrap_or(0);
                let sessions = store
                    .list_sessions_filtered(
                        source.as_deref(),
                        project.as_deref(),
                        limit + 1,
                        offset,
                    )
                    .map_err(internal)?;
                let has_more = sessions.len() > limit;
                serde_json::json!({"sessions":sessions.into_iter().take(limit).collect::<Vec<_>>(),
                    "next_offset":has_more.then(|| offset.saturating_add(limit))})
            }
            "stats" => serde_json::to_value(
                store
                    .stats_window(&flare_insights::store_query::StatsWindow {
                        since: req
                            .since_days
                            .map(|d| chrono::Utc::now() - chrono::Duration::days(i64::from(d))),
                        source,
                        project_prefix: project,
                    })
                    .map_err(internal)?,
            )
            .map_err(internal)?,
            "export" | "handoff" => super::insights_manage::document(
                &store,
                &req,
                source.as_deref(),
                project.as_deref(),
            )?,
            "search" => {
                let query = req.query.clone().unwrap_or_default();
                if query.trim().is_empty() {
                    return Err(invalid("query must not be empty"));
                }
                let hits = flare_insights::search::search(
                    &store,
                    &flare_insights::search::SearchOptions {
                        query,
                        source,
                        project,
                        limit: req.limit.unwrap_or(20).min(MAX_SEARCH_LIMIT),
                        offset: req.offset.unwrap_or(0),
                        ..Default::default()
                    },
                )
                .map_err(internal)?;
                serde_json::json!({ "hits": hits })
            }
            "recall" | "show" => {
                let id = req
                    .session_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| invalid("session_id is required for recall"))?;
                let not_found = || invalid(format!("session '{id}' not found in insights index"));
                let page = flare_insights::recall::recall_page_scrubbed(
                    &store,
                    source.as_deref(),
                    id,
                    req.turn_start.unwrap_or(0),
                    req.turn_limit
                        .unwrap_or(flare_insights::recall::DEFAULT_TURN_LIMIT)
                        .min(MAX_TURN_LIMIT),
                    req.max_chars
                        .unwrap_or(flare_insights::recall::DEFAULT_MAX_CHARS)
                        .min(MAX_RECALL_CHARS),
                    &redact_value,
                )
                .map_err(internal)?
                .ok_or_else(not_found)?;
                // Same path-prefix rule as search; `project: "*"` leaves it None.
                if let Some(prefix) = project.as_deref() {
                    let in_scope = store
                        .sessions_in_window(None, None, Some(prefix), Some(&page.session.id))
                        .map_err(internal)?;
                    if in_scope.is_empty() {
                        return Err(not_found());
                    }
                }
                let mut data = serde_json::to_value(&page).map_err(internal)?;
                if action == "show" {
                    let limit = req.limit.unwrap_or(20).clamp(1, MAX_SEARCH_LIMIT);
                    let offset = req.offset.unwrap_or(0);
                    let files = store.get_file_events(&page.session.id).map_err(internal)?;
                    let subagents = store.get_subagents(&page.session.id).map_err(internal)?;
                    data["file_events"] = serde_json::to_value(
                        files.iter().skip(offset).take(limit).collect::<Vec<_>>(),
                    )
                    .map_err(internal)?;
                    data["subagents"] = serde_json::to_value(
                        subagents
                            .iter()
                            .skip(offset)
                            .take(limit)
                            .collect::<Vec<_>>(),
                    )
                    .map_err(internal)?;
                    data["evidence_pagination"] = serde_json::json!({"offset":offset,"limit":limit,"file_events_total":files.len(),"subagents_total":subagents.len(),"next_offset":(offset.saturating_add(limit)<files.len().max(subagents.len())).then(||offset.saturating_add(limit))});
                }
                data
            }
            "forensics" => {
                let report = flare_insights::forensics::compute(
                    &store,
                    &flare_insights::forensics::ForensicsOptions {
                        days: req.since_days.unwrap_or(7),
                        project_prefix: project,
                        source,
                        session_id: req.session_id.clone().filter(|s| !s.trim().is_empty()),
                    },
                    chrono::Utc::now(),
                )
                .map_err(internal)?;
                serde_json::to_value(report).map_err(internal)?
            }
            "discover" => {
                if source.is_some() {
                    return Err(invalid(
                        "insights action 'discover' does not support source",
                    ));
                }
                let report = flare_insights::discover::mine(
                    &store,
                    &flare_insights::discover::DiscoverOptions {
                        days: req.since_days.unwrap_or(30),
                        project_prefix: project,
                        min_sessions: req.min_sessions.unwrap_or(2).max(1),
                        min_count: req.min_count.unwrap_or(3).max(1),
                        jaccard_threshold: flare_insights::discover::DEFAULT_JACCARD_THRESHOLD,
                    },
                    chrono::Utc::now(),
                )
                .map_err(internal)?;
                serde_json::to_value(report).map_err(internal)?
            }
            _ => unreachable!("action validated above"),
        };
        if matches!(action.as_str(), "forensics" | "discover") {
            report_page(&mut data, &req);
        }
        envelope(&action, data)
    }

    /// `search type=session`: insights hits in the shape every other search type
    /// returns (`query`/`source`/`total`/`groups`), keeping the provenance notice.
    pub(super) fn search_sessions(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        if req.meta.is_some() || req.path_glob.is_some() {
            return Err(invalid(
                "search type=session does not support meta or path_glob",
            ));
        }
        let out = self.insights_impl(InsightsRequest {
            action: "search".into(),
            query: Some(req.query.clone()),
            limit: req.limit,
            ..Default::default()
        })?;
        let parsed: Value = serde_json::from_str(&out).map_err(internal)?;
        let empty = Vec::new();
        let hits = parsed["data"]["hits"].as_array().unwrap_or(&empty);
        let entries: Vec<Value> = hits
            .iter()
            .map(|hit| (hit, -hit["rank"].as_f64().unwrap_or(0.0)))
            .filter(|(_, score)| req.min_score.is_none_or(|min| *score >= min))
            .enumerate()
            .map(|(rank, (hit, score))| {
                let s = &hit["session"];
                serde_json::json!({
                    "id": s["id"], "title": s["title"], "source": s["source"],
                    "project": s["project"], "updated_at": s["updated_at"],
                    "turn_seq": hit["turn_seq"], "snippet": hit["snippet"],
                    "score": score, "rank": rank,
                })
            })
            .collect();
        serde_json::to_string_pretty(&serde_json::json!({
            "query": req.query.trim(),
            "source": "session",
            "provenance": PROVENANCE,
            "scope": "current directory; use the insights tool with project=\"*\" for all projects",
            "total": entries.len(),
            "groups": { "session": entries },
        }))
        .map_err(internal)
    }
}

#[cfg(test)]
mod report_tests {
    use super::*;

    #[test]
    fn long_first_draft_does_not_empty_later_previews() {
        let mut data = serde_json::json!({"body":[
            {"examples":[{"excerpt":"x".repeat(2000)}],"draft":"a".repeat(2000)},
            {"examples":[{"excerpt":"y".repeat(2000)}],"draft":"b".repeat(2000)},
            {"examples":[{"excerpt":"z".repeat(2000)}],"draft":"c".repeat(2000)}
        ]});
        report_page(
            &mut data,
            &InsightsRequest {
                max_chars: Some(1200),
                ..Default::default()
            },
        );
        for item in data["body"].as_array().unwrap() {
            assert_eq!(item["draft"].as_str().unwrap().chars().count(), 400);
        }
        assert_eq!(data["omissions"]["/body/2/draft"], 1600);
    }
}
