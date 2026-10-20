//! Read-only evidence projection over the existing workflow journal and insights index.

use std::collections::BTreeSet;
use std::path::Path;

use chrono::{DateTime, Utc};
use flare_insights::model::ToolCall;
use flare_insights::store::InsightsStore;
use flare_workflow::{EntryResult, JournalEntry, WorkflowRunId};
use serde_json::{Value, json};

pub fn workflow_report(run_id: &str, db_path: &Path) -> Result<Value, String> {
    super::blocking_runtime().block_on(workflow_report_async(run_id, db_path))
}

pub(crate) async fn workflow_report_async(run_id: &str, db_path: &Path) -> Result<Value, String> {
    let run_id = super::parse_run_id(run_id)?;
    let db_path = db_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        read_report(run_id, &db_path, &crate::handoff::default_insights_db())
    })
    .await
    .map_err(|e| format!("evidence report task: {e}"))?
}

fn read_report(
    run_id: WorkflowRunId,
    db_path: &Path,
    insights_path: &Path,
) -> Result<Value, String> {
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("open workflow evidence: {e}"))?;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let state = super::load_state_from_conn(&run_id, &tx)?;
    let journal =
        flare_workflow::journal::read(&tx, &run_id.to_string()).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    let insights = if insights_path.exists() {
        Some(
            InsightsStore::open_read_only(insights_path)
                .map_err(|e| format!("open session evidence: {e}"))?,
        )
    } else {
        None
    };
    build_report(&state, &journal, insights.as_ref())
}

fn remember_sessions(data: &Value, ids: &mut BTreeSet<String>) {
    if let Some(id) = data["session_id"].as_str().filter(|id| !id.is_empty()) {
        ids.insert(id.into());
    }
    if let Some(sessions) = data["agent_sessions"].as_object() {
        ids.extend(
            sessions
                .values()
                .filter_map(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_string),
        );
    }
    if let Some(sessions) = data["session_ids"].as_array() {
        ids.extend(
            sessions
                .iter()
                .filter_map(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_string),
        );
    }
}

fn timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value.as_str()?.parse().ok()
}

fn in_run(
    at: Option<DateTime<Utc>>,
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
) -> bool {
    matches!((at, start, end), (Some(at), Some(start), Some(end)) if at >= start && at <= end)
}

fn verification(tool: &ToolCall) -> Option<Value> {
    let response = tool
        .output
        .as_ref()
        .map(|s| serde_json::from_str(s).unwrap_or_else(|_| json!(s)))
        .unwrap_or(Value::Null);
    let parsed = crate::hook_completion_gate::parse_post_tool_use(&json!({
        "session_id": tool.session_id, "tool_name": tool.name, "tool_input": tool.input, "tool_response": response,
    }).to_string())?;
    let command = parsed.command?;
    if !crate::optimize::is_verification_command(&command) {
        return None;
    }
    let outcome = if parsed.is_running || tool.output.is_none() || tool.status == "pending" {
        "unknown"
    } else if parsed.is_error || tool.status == "failed" || tool.status == "error" {
        "error"
    } else {
        match parsed.exit_code {
            Some(0) => "passed",
            Some(_) => "failed",
            None => "unknown",
        }
    };
    Some(
        json!({"tool_call_id": tool.id, "command": command, "outcome": outcome, "exit_code": parsed.exit_code, "output": tool.output, "at": tool.created_at}),
    )
}

fn build_report(
    state: &Value,
    journal: &[JournalEntry],
    insights: Option<&InsightsStore>,
) -> Result<Value, String> {
    let data = &state["context"]["data"];
    let start = timestamp(&state["created_at"]);
    let end = timestamp(&state["updated_at"]);
    let mut ids = BTreeSet::new();
    remember_sessions(data, &mut ids);
    let mut entries = Vec::new();
    for (index, entry) in journal.iter().enumerate() {
        match entry {
            JournalEntry::StepRun { step_id, attempt, result } | JournalEntry::Rollback { step_id, attempt, result } => {
                let (outcome, failure) = match result {
                    Some(EntryResult::Failure { code, message, metadata }) => ("failed", json!({"code":code,"message":message,"metadata":metadata})),
                    Some(EntryResult::Success(bytes)) => {
                        if let Ok(context) = serde_json::from_slice::<Value>(bytes) { remember_sessions(&context["data"], &mut ids); }
                        ("succeeded", Value::Null)
                    }
                    None => ("pending", Value::Null),
                };
                entries.push(json!({"journal_index":index,"kind":entry.entry_type(),"step_id":step_id,"attempt":attempt,"execution_outcome":outcome,"failure":failure}));
            }
            JournalEntry::LoopIteration { step_id, iteration, .. } => entries.push(json!({"journal_index":index,"kind":"loop_iteration","step_id":step_id,"iteration":iteration})),
            _ => {}
        }
    }
    let mut warnings = Vec::new();
    if ids.is_empty() {
        warnings.push(
            "No provider session links were recorded; tool and cost evidence is unavailable."
                .to_string(),
        );
    }
    if insights.is_none() {
        warnings.push(
            "Insights index unavailable; run agentflare insights sync to populate it.".into(),
        );
    }
    let mut sessions = Vec::new();
    for id in ids {
        let Some(store) = insights else {
            sessions.push(json!({"requested_session_id":id,"availability":"unknown"}));
            continue;
        };
        let Some(session) = store
            .get_session_by_provider_id(&id)
            .map_err(|e| format!("session evidence {id}: {e}"))?
        else {
            warnings.push(format!("Session {id} is missing or ambiguous in the index; use insights sync --force to refresh historical records."));
            sessions.push(json!({"requested_session_id":id,"availability":"unknown"}));
            continue;
        };
        let same_worktree = data["worktree_path"]
            .as_str()
            .filter(|p| !p.is_empty())
            .zip(session.cwd.as_deref())
            .is_some_and(|(expected, actual)| Path::new(expected) == Path::new(actual));
        if !same_worktree {
            warnings.push(format!(
                "Session {id} has no matching worktree provenance; its contents were excluded."
            ));
            sessions.push(json!({"requested_session_id":id,"availability":"unverified"}));
            continue;
        }
        let turns = store.get_turns(&session.id).map_err(|e| e.to_string())?;
        let tools = store
            .get_tool_calls(&session.id)
            .map_err(|e| e.to_string())?;
        let files = store
            .get_file_events(&session.id)
            .map_err(|e| e.to_string())?;
        let coverage = json!({
            "tools_total_in_session":tools.len(), "tools_without_timestamp":tools.iter().filter(|t|t.created_at.is_none()).count(),
            "turns_total_in_session":turns.len(), "turns_without_timestamp":turns.iter().filter(|t|t.started_at.is_none()).count(),
            "files_total_in_session":files.len(),
        });
        let tools: Vec<_> = tools
            .into_iter()
            .filter(|t| in_run(t.created_at, start, end))
            .collect();
        let checks: Vec<_> = tools.iter().filter_map(verification).collect();
        let turns: Vec<_> = turns
            .into_iter()
            .filter(|t| in_run(t.started_at, start, end))
            .collect();
        let files: Vec<_> = files
            .into_iter()
            .filter(|f| in_run(Some(f.at), start, end))
            .collect();
        sessions.push(json!({
            "requested_session_id":id,"session_id":session.id,"source":session.source,"model":session.model,"availability":"available",
            "source_updated_at":session.updated_at,"session_cost_usd":session.cost.map(|c|c.total_usd),
            "cost_scope":"entire session; insights value may be estimated, not run-attributed spending",
            "coverage":coverage,"observed_checks":checks,"events":{"turns":turns,"tool_calls":tools,"file_activity":files},
        }));
    }
    let mut run = super::status_view(state, journal);
    run["created_at"] = state["created_at"].clone();
    run["updated_at"] = state["updated_at"].clone();
    run["observed_elapsed_ms"] = json!(
        start
            .zip(end)
            .map(|(s, e)| (e - s).num_milliseconds().max(0))
    );
    // shortcut: session totals span runs; budget receipts are provider reports, not a verified total bill.
    // shortcut: file activity is not an artifact diff; require stored snapshot IDs before comparing artifacts.
    let mut report = json!({
        "schema_version":1,"generated_at":Utc::now(),"run":run,
        "acceptance":{"status":"unknown","reason":"Execution completion and observed checks do not establish all task acceptance criteria or coverage of the final tree."},
        "journal_evidence":entries,"sessions":sessions,"run_cost_usd":Value::Null,
        "work_item":{"item_id":data["item_id"],"owner":data["owner"],"worktree_path":data["worktree_path"],"pr_url":data["pr_url"],"tasks":data["tasks"]},
        "agent_claims":{"summary":data["reply_text"],"ledger":data["ledger"],"review_issues":data["review_issues"],"review_findings":data["review_findings"],"last_report":data["last_report"],"hold_reason":data["hold_reason"]},
        "coverage":{"warnings":warnings,"scope":"Persisted run state/journal plus explicitly linked session events within run timestamps.",
            "limits":["Workflow attempts are not provider invocation counts; failed-call usage may be missing.","Per-step token/duration counters cover the last successful attempt.","Session links and indexed events can be incomplete; refresh with insights sync --force.","File events record tool requests, not verified artifact changes; no immutable Git diff was recorded.","Agent claims are not independently verified acceptance results."]},
    });
    let truncated = sanitize(&mut report);
    report["coverage"]["truncated_text_fields"] = json!(truncated);
    Ok(report)
}

fn sanitize(value: &mut Value) -> usize {
    match value {
        Value::String(text) => {
            let mut nested_truncated = 0;
            let boundary = if text.contains("\r\nOutput:\r\n") {
                "\r\nOutput:\r\n"
            } else {
                "\nOutput:\n"
            };
            let (header, payload) = text
                .split_once(boundary)
                .map(|(head, body)| (Some(head), body))
                .unwrap_or((None, text.as_str()));
            let structured = serde_json::from_str::<Value>(payload)
                .ok()
                .filter(|v| v.is_object() || v.is_array());
            let redacted = if let Some(mut structured) = structured {
                nested_truncated = sanitize(&mut structured);
                match header {
                    Some(header) => format!(
                        "{}{boundary}{structured}",
                        crate::handoff::body::redact(header)
                    ),
                    None => structured.to_string(),
                }
            } else {
                crate::handoff::body::redact(text)
            };
            let truncated =
                usize::from(redacted.chars().count() > crate::handoff::body::MAX_TURN_CHARS);
            *text = crate::handoff::body::truncate(&redacted, crate::handoff::body::MAX_TURN_CHARS);
            truncated + nested_truncated
        }
        Value::Array(values) => values.iter_mut().map(sanitize).sum(),
        Value::Object(values) => values
            .iter_mut()
            .map(|(key, value)| {
                if crate::handoff::body::has_sensitive_name(&format!("{key}=")) {
                    *value = json!("[REDACTED]");
                    0
                } else {
                    sanitize(value)
                }
            })
            .sum(),
        _ => 0,
    }
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
