//! `item(action="review_result")` -- how a dispatched agent reports what it
//! did about one review-bot thread the supervisor handed it (see
//! `supervisor::review_bots`). The result is recorded on the item's
//! metadata (read-merge-write under `merge_item_metadata`, so a concurrent
//! writer's keys survive) and the next review sweep replies on the thread,
//! resolving it when the fix is confirmed on the remote. The agent never
//! replies on GitHub itself: one reply per thread per round, with the
//! supervisor's idempotency marker, is the whole point.

use super::*;
use crate::supervisor::review_findings::{
    ReviewOutcome, ReviewResult, insert_thread_result, thread_record,
};

/// Whether `sha` could name a commit: 7-40 hex digits.
fn plausible_sha(sha: &str) -> bool {
    let s = sha.trim();
    (7..=40).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
}

impl AgentflareMcp {
    pub(crate) fn item_review_result(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let raw = req
            .id
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| ErrorData::invalid_params("id is required for review_result", None))?;
        let thread_id = req
            .thread_id
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                ErrorData::invalid_params("thread_id is required for review_result", None)
            })?;
        let outcome = req
            .outcome
            .as_deref()
            .and_then(ReviewOutcome::parse)
            .ok_or_else(|| {
                ErrorData::invalid_params(
                    "outcome is required for review_result: fixed|not_valid|out_of_scope|skipped",
                    None,
                )
            })?;
        let sha = req
            .sha
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());
        if outcome == ReviewOutcome::Fixed {
            match sha.as_deref() {
                Some(s) if plausible_sha(s) => {}
                Some(_) => {
                    return Err(ErrorData::invalid_params(
                        "sha must be a 7-40 hex-digit commit sha",
                        None,
                    ));
                }
                None => {
                    return Err(ErrorData::invalid_params(
                        "sha is required when outcome=fixed: the commit carrying the fix",
                        None,
                    ));
                }
            }
        }
        let note = req
            .note
            .or(req.reason)
            .map(|n| n.trim().chars().take(1500).collect::<String>())
            .unwrap_or_default();
        if outcome != ReviewOutcome::Fixed && note.is_empty() {
            return Err(ErrorData::invalid_params(
                "note is required when the finding stays as is: say why",
                None,
            ));
        }
        let test = req
            .test
            .map(|t| t.trim().chars().take(300).collect::<String>())
            .filter(|t| !t.is_empty());
        let now = crate::claims::now();
        let owner = crate::claims::owner_id();
        self.with_backend_db(|conn| {
            let id = self.resolve_item_id(conn, &raw)?;
            let mut round = 0;
            merge_item_metadata(conn, &id, |meta| {
                round = thread_record(meta, &thread_id).round.max(1);
                insert_thread_result(
                    meta,
                    &thread_id,
                    &ReviewResult {
                        round,
                        outcome,
                        sha: sha.clone(),
                        note: note.clone(),
                        test: test.clone(),
                    },
                    now,
                );
                crate::supervisor::review_attempts::record_outcome(
                    meta, &owner, &thread_id, round, outcome,
                );
            })
            .map_err(map_backend_err)?;
            Ok(serde_json::json!({
                "item_id": id,
                "thread_id": thread_id,
                "outcome": outcome.as_str(),
                "round": round,
                "recorded": true,
                "next": match outcome {
                    ReviewOutcome::Fixed => "the supervisor replies on the thread and resolves it once this commit is on the PR branch -- push it if you have not",
                    _ => "the supervisor replies on the thread and leaves it open for the reviewer",
                },
            })
            .to_string())
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::review_findings::{REVIEW_THREADS_KEY, thread_result};

    fn seeded(mcp: &AgentflareMcp) -> String {
        mcp.with_backend_db(|conn| {
            let project = mcp.resolve_project(conn).unwrap();
            let states = agentflare_backend::state::list_by_project(conn, &project.id).unwrap();
            let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
            agentflare_backend::item::create(
                conn,
                agentflare_backend::item::CreateItem {
                    project_id: project.id.clone(),
                    state_id,
                    name: "Fix review".into(),
                    description: None,
                    priority: None,
                    parent_id: None,
                    assignee_agent: None,
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                    metadata: Some(
                        serde_json::json!({ REVIEW_THREADS_KEY: { "PRRT_1": { "round": 2 } }, "keep": 1 })
                            .to_string(),
                    ),
                    label_ids: vec![],
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                    start_date: None,
                    due_date: None,
                },
            )
            .unwrap()
            .id
        })
        .unwrap()
    }

    fn req(id: &str, thread: &str, outcome: &str) -> ItemRequest {
        ItemRequest {
            action: "review_result".into(),
            id: Some(id.into()),
            thread_id: Some(thread.into()),
            outcome: Some(outcome.into()),
            ..Default::default()
        }
    }

    fn item724_attempt(mcp: &AgentflareMcp) -> String {
        let id = seeded(mcp);
        mcp.with_backend_db(|conn| {
            merge_item_metadata(conn, &id, |meta| {
                crate::supervisor::review_findings::set_thread_round(meta, "PRRT_2", 2);
                meta.insert(
                    "coderabbit_repair_attempt".into(),
                    serde_json::json!({
                        "job_id": "repair-job", "threads": {"PRRT_1": 2, "PRRT_2": 2},
                        "outcomes": {}, "credited": false
                    }),
                );
            })
            .unwrap();
        })
        .unwrap();
        id
    }

    fn item724_credits(mcp: &AgentflareMcp, id: &str) -> u64 {
        mcp.with_backend_db(|conn| {
            let item = agentflare_backend::item::get(conn, id).unwrap();
            metadata_object(&item.metadata)
                .get("coderabbit_repair_noop_credits")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        })
        .unwrap()
    }

    #[test]
    fn item724_noop_batch_is_credited_once_and_a_corrected_fix_reverses_credit() {
        let mcp = AgentflareMcp::for_test_memory();
        let id = item724_attempt(&mcp);
        crate::claims::with_owner_override("codex:repair-job", || {
            let mut first = req(&id, "PRRT_1", "out_of_scope");
            first.note = Some("path is outside the PR diff".into());
            mcp.item_review_result(first).unwrap();
            assert_eq!(
                item724_credits(&mcp, &id),
                0,
                "partial results must stay charged"
            );
            for _ in 0..2 {
                let mut second = req(&id, "PRRT_2", "skipped");
                second.note = Some("optional finding; no change made".into());
                mcp.item_review_result(second).unwrap();
                assert_eq!(item724_credits(&mcp, &id), 1);
            }
            let mut corrected = req(&id, "PRRT_2", "fixed");
            corrected.sha = Some("abcdef1234567".into());
            corrected.note = Some("a fix was made after all".into());
            mcp.item_review_result(corrected).unwrap();
            assert_eq!(
                item724_credits(&mcp, &id),
                0,
                "mixed/fixed batches remain charged"
            );
        });
    }

    #[test]
    fn item724_stale_dispatch_owner_cannot_credit_the_current_attempt() {
        let mcp = AgentflareMcp::for_test_memory();
        let id = item724_attempt(&mcp);
        crate::claims::with_owner_override("codex:old-job", || {
            for thread in ["PRRT_1", "PRRT_2"] {
                let mut result = req(&id, thread, "out_of_scope");
                result.note = Some("old dispatch report".into());
                mcp.item_review_result(result).unwrap();
            }
        });
        assert_eq!(item724_credits(&mcp, &id), 0);
    }

    #[test]
    fn review_result_records_the_result_on_the_dispatched_round() {
        let mcp = AgentflareMcp::for_test_memory();
        let id = seeded(&mcp);
        let mut r = req(&id, "PRRT_1", "fixed");
        r.sha = Some("ABCDEF1234567".into());
        r.note = Some("checked indexing".into());
        r.test = Some("no_panic_on_empty".into());
        let out: serde_json::Value =
            serde_json::from_str(&mcp.item_review_result(r).unwrap()).unwrap();
        assert_eq!(out["round"], 2, "the round the thread was dispatched on");
        let item = mcp
            .with_backend_db(|conn| agentflare_backend::item::get(conn, &id).unwrap())
            .unwrap();
        let meta = metadata_object(&item.metadata);
        assert_eq!(meta["keep"], 1, "unrelated metadata survives");
        let res = thread_result(&meta, "PRRT_1").expect("result recorded");
        assert_eq!(res.round, 2);
        assert_eq!(res.outcome, ReviewOutcome::Fixed);
        assert_eq!(res.sha.as_deref(), Some("abcdef1234567"));
        assert_eq!(res.test.as_deref(), Some("no_panic_on_empty"));
    }

    #[test]
    fn review_result_requires_a_sha_for_a_fix_and_a_note_otherwise() {
        let mcp = AgentflareMcp::for_test_memory();
        let id = seeded(&mcp);
        let err = mcp
            .item_review_result(req(&id, "PRRT_1", "fixed"))
            .unwrap_err();
        assert!(err.message.contains("sha is required"));
        let mut bad = req(&id, "PRRT_1", "fixed");
        bad.sha = Some("not-a-sha".into());
        assert!(mcp.item_review_result(bad).is_err());
        let err = mcp
            .item_review_result(req(&id, "PRRT_1", "not_valid"))
            .unwrap_err();
        assert!(err.message.contains("note is required"));
        let err = mcp
            .item_review_result(req(&id, "PRRT_1", "maybe"))
            .unwrap_err();
        assert!(err.message.contains("outcome"));
    }

    #[test]
    fn review_result_defaults_an_undispatched_thread_to_round_one() {
        let mcp = AgentflareMcp::for_test_memory();
        let id = seeded(&mcp);
        let mut r = req(&id, "review:9", "out-of-scope");
        r.reason = Some("belongs in the follow-up PR".into());
        let out: serde_json::Value =
            serde_json::from_str(&mcp.item_review_result(r).unwrap()).unwrap();
        assert_eq!(out["round"], 1);
        assert_eq!(out["outcome"], "out_of_scope");
    }
}
