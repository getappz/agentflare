//! Per-site feature extraction. Everything is truncated here; `build_row`
//! redacts and enforces the global size bound on top.
use serde_json::{Value, json};

const ROUTER_PROMPT_CHARS: usize = 500;
const RERANK_PROMPT_CHARS: usize = 500;
const RERANK_DESC_CHARS: usize = 120;
const JUDGE_REPLY_CHARS: usize = 500;
const RERANK_MAX_CANDIDATES: usize = 10;

/// Redact the FULL text, then clip, so a secret straddling the cut can't leak a prefix.
fn clip(s: &str, max: usize) -> String {
    crate::mcp_server::secret_scan::redact(s)
        .chars()
        .take(max)
        .collect()
}

/// Router: truncated prompt, length and simple counts.
pub fn router_features(prompt: &str) -> Value {
    json!({
        "prompt": clip(prompt, ROUTER_PROMPT_CHARS),
        "len": prompt.chars().count(),
        "code_fences": prompt.matches("```").count() / 2,
        "paths": prompt
            .split_whitespace()
            .filter(|w| w.contains('/') || w.contains('\\'))
            .count(),
        "question_marks": prompt.matches('?').count(),
    })
}

/// Skill rerank: truncated prompt plus each candidate's name, description,
/// BM25 score and rank (0 = best).
pub fn rerank_features<'a>(
    prompt: &str,
    candidates: impl IntoIterator<Item = (&'a str, &'a str, f64)>,
) -> Value {
    let cands: Vec<Value> = candidates
        .into_iter()
        .take(RERANK_MAX_CANDIDATES)
        .enumerate()
        .map(|(rank, (name, desc, score))| {
            json!({
                "name": name,
                "description": clip(desc, RERANK_DESC_CHARS),
                "score": score,
                "rank": rank,
            })
        })
        .collect();
    json!({ "prompt": clip(prompt, RERANK_PROMPT_CHARS), "candidates": cands })
}

/// SDD judge: the structured signals of `judge_shadow_state` (mode, plan
/// shape, review markers) plus a clipped reply, never the transcript.
/// Returns `(features, norm_input)`; the key covers the structure plus the
/// reply head so repeated judge states collapse.
pub fn judge_features(state: &Value) -> (Value, String) {
    let reply = state["latest_role_reply"].as_str().unwrap_or("");
    let lower = reply.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    let markers = json!({
        "approved": has(&["lgtm", "approved", "looks good"]),
        "issues": has(&["must fix", "blocking", "needs changes", "bug"]),
        "error": has(&["error", "failed", "panic"]),
        "tests_pass": has(&["tests pass", "all tests", "passed"]),
    });
    let mode = state["mode"].as_str().unwrap_or("");
    let plan_len = state["plan"].as_array().map_or(0, Vec::len);
    let ledger_len = state["ledger_tail"].as_array().map_or(0, Vec::len);
    let current = state["current_task"]["title"].as_str().unwrap_or("");
    let features = json!({
        "mode": mode,
        "plan_len": plan_len,
        "ledger_len": ledger_len,
        "current_task": clip(current, 120),
        "markers": markers,
        "reply": clip(reply, JUDGE_REPLY_CHARS),
    });
    let norm_input = format!(
        "{mode}|{plan_len}|{}|{markers}|{}",
        clip(current, 120),
        clip(reply, 200)
    );
    (features, norm_input)
}
