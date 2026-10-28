//! Credits only fully reported no-op dispatches; lost jobs still consume the cap.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::review_findings::{
    BotFinding, ReviewOutcome, ReviewResult, insert_thread_result, thread_record, thread_result,
};

const ATTEMPT: &str = "coderabbit_repair_attempt";
const CREDITS: &str = "coderabbit_repair_noop_credits";
const NOOP_CREDIT_CEILING: u32 = crate::quota::decide::SELF_REPAIR_CAP;

#[derive(Deserialize, Serialize)]
struct Attempt {
    job_id: String,
    threads: BTreeMap<String, u32>,
    outcomes: BTreeMap<String, String>,
    credited: bool,
}

pub(super) fn begin_attempt(meta: &mut Map<String, Value>, job_id: &str, findings: &[BotFinding]) {
    let attempt = Attempt {
        job_id: job_id.into(),
        threads: findings
            .iter()
            .map(|f| (f.thread_id.clone(), f.round()))
            .collect(),
        outcomes: BTreeMap::new(),
        credited: false,
    };
    meta.insert(
        ATTEMPT.into(),
        serde_json::to_value(attempt).expect("serializable attempt"),
    );
}

pub(crate) fn record_outcome(
    meta: &mut Map<String, Value>,
    owner: &str,
    thread: &str,
    round: u32,
    outcome: ReviewOutcome,
) {
    let Some(mut attempt) = meta
        .get(ATTEMPT)
        .cloned()
        .and_then(|v| serde_json::from_value::<Attempt>(v).ok())
    else {
        return;
    };
    // The job owner is propagated to child agents; a late report cannot refund a newer job.
    if owner.rsplit_once(':').map(|(_, id)| id) != Some(attempt.job_id.as_str())
        || attempt.threads.get(thread) != Some(&round)
    {
        return;
    }
    attempt
        .outcomes
        .insert(thread.into(), outcome.as_str().into());
    let noop = !attempt.threads.is_empty()
        && attempt.threads.keys().all(|id| {
            attempt
                .outcomes
                .get(id)
                .and_then(|o| ReviewOutcome::parse(o))
                .is_some_and(|o| o != ReviewOutcome::Fixed)
        });
    let credits = meta.get(CREDITS).and_then(Value::as_u64).unwrap_or(0);
    let credits = match (attempt.credited, noop) {
        (false, true) => credits.saturating_add(1),
        (true, false) => credits.saturating_sub(1),
        _ => credits,
    };
    attempt.credited = noop;
    meta.insert(CREDITS.into(), credits.into());
    meta.insert(
        ATTEMPT.into(),
        serde_json::to_value(attempt).expect("serializable attempt"),
    );
}

/// Explicit parking still owes a reply for every finding the job did not report.
pub(crate) fn record_parked(meta: &mut Map<String, Value>, owner: &str, reason: &str, now: i64) {
    let Some(attempt) = meta
        .get(ATTEMPT)
        .cloned()
        .and_then(|v| serde_json::from_value::<Attempt>(v).ok())
    else {
        return;
    };
    if owner.rsplit_once(':').map(|(_, id)| id) != Some(attempt.job_id.as_str()) {
        return;
    }
    for (thread, round) in &attempt.threads {
        if attempt.outcomes.contains_key(thread)
            || thread_record(meta, thread).round != *round
            || thread_result(meta, thread).is_some_and(|r| r.round.max(1) == *round)
        {
            continue;
        }
        insert_thread_result(
            meta,
            thread,
            &ReviewResult {
                round: *round,
                outcome: ReviewOutcome::Skipped,
                sha: None,
                note: format!("Repair parked before reporting this finding: {reason}")
                    .chars()
                    .take(1500)
                    .collect(),
                test: None,
            },
            now,
        );
        record_outcome(meta, owner, thread, *round, ReviewOutcome::Skipped);
    }
}

pub(super) fn charged_attempts(meta: &Map<String, Value>, gross: u32) -> u32 {
    gross.saturating_sub(
        meta.get(CREDITS)
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u64::from(NOOP_CREDIT_CEILING)) as u32,
    )
}
