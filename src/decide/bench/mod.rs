//! `agentflare decide bench <site>`: replay captured/labeled inputs through
//! Jev (a [`Backend`]) and the site's non-Jev baseline, and report one table.
//!
//! Inputs are JSONL [`Case`]s: either rows of `AGENTFLARE_DECIDE_CAPTURE=1`
//! (`site`, `features`, `baseline`, `label`, `confidence`) or hand-labeled
//! fixtures that add `expected` / `adversarial` / `id`. The baseline answer is
//! the one recorded next to the input; Jev's answer comes from the backend.
//! The shipped backend is a deterministic fake (replays the recorded label
//! with a seeded virtual latency), so CI needs no network and p50/p95 are
//! reproducible. Real runs plug a live backend into the same trait.
//!
//! # Token counting (identical for every site)
//! `tokens(text) = ceil(chars(text) / 4)`. Jev tokens *added* per decision =
//! `tokens(compact JSON of features) + tokens(answer label)`. Tokens
//! *avoided* = the site's `llm_tokens_avoided` (thresholds file) for each
//! decision whose Jev answer was applied (answered and confidence >=
//! `min_confidence`). The heuristic is deliberately crude but is the same
//! function everywhere, so sites compare.
//!
//! # Metrics
//! - agreement: share of replayed cases where the final decision equals the
//!   baseline answer.
//! - precision / recall: macro-averaged over the expected classes of labeled
//!   cases (a class with no predictions has precision 0), for Jev's final
//!   decision and for the baseline.
//! - latency: nearest-rank p50/p95 in ms; cold = first `cold_calls` calls of
//!   a set, warm = the rest.
//! - overhead: harness time outside backend calls / summed backend latency.
//!
//! # What the numbers mean
//! - Baseline: the answer the site's non-Jev path produced when the row was
//!   captured (`baseline`). The bench replays it; it does not re-run the
//!   site's code (the sites decide inline inside hooks, with no pure entry
//!   point), so a baseline regression is invisible here.
//! - Backends: `FakeBackend` (default) replays the recorded Jev label with
//!   *virtual* latency, so its report is `synthetic` and measures the
//!   harness only. `LiveBackend` (`--live`) calls Jev with each case's
//!   captured question and sanitized features; only those reports measure
//!   the sites.
//! - Overhead %: real harness microseconds divided by summed backend latency.
//!   With the fake backend the denominator is the virtual profile, so the
//!   figure scales with `--fake-*-ms`; the < 5% acceptance is stated for the
//!   default profile (1200/350 ms). With `--live` it is against wall time.
mod backend;
mod live;
mod metrics;
mod render;
mod thresholds;

#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Instant;

pub use backend::{Backend, Failure, FailureMode, FakeBackend, Profile, Reply};
pub use live::LiveBackend;
pub use metrics::{Pct, Prf, count_tokens, percentile};
pub use render::{render_json, render_table};
pub use thresholds::{SiteThresholds, Thresholds, Verdict};

pub const SITES: [&str; 3] = ["router", "skill_rerank", "sdd_judge"];

const BUILTIN: [(&str, &str); 3] = [
    (
        "router",
        include_str!("../../../tests/fixtures/decide_bench/router.jsonl"),
    ),
    (
        "skill_rerank",
        include_str!("../../../tests/fixtures/decide_bench/skill_rerank.jsonl"),
    ),
    (
        "sdd_judge",
        include_str!("../../../tests/fixtures/decide_bench/sdd_judge.jsonl"),
    ),
];

/// One replayable decision input.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Case {
    #[serde(default)]
    pub id: Option<String>,
    pub site: String,
    #[serde(default)]
    pub features: Value,
    /// The non-Jev answer recorded for this input.
    pub baseline: String,
    /// Jev's recorded answer (`{"summary": ..}` or a bare string); what the
    /// fake backend replays.
    #[serde(default)]
    pub label: Option<Value>,
    /// Typed questions as captured (`BTreeMap<String, Question>`); the live
    /// backend needs it.
    #[serde(default)]
    pub question: Option<Value>,
    #[serde(default)]
    pub confidence: Option<f64>,
    /// Ground truth, when labeled.
    #[serde(default)]
    pub expected: Option<String>,
    #[serde(default)]
    pub adversarial: bool,
}

impl Case {
    pub fn recorded_label(&self) -> Option<String> {
        match self.label.as_ref()? {
            Value::String(s) => Some(s.clone()),
            v => v.get("summary").and_then(Value::as_str).map(str::to_owned),
        }
    }
}

/// Parse JSONL; damaged lines are skipped and counted.
pub fn parse_cases(text: &str) -> (Vec<Case>, usize) {
    let mut bad = 0;
    let cases = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| match serde_json::from_str::<Case>(l) {
            Ok(c) => Some(c),
            Err(_) => {
                bad += 1;
                None
            }
        })
        .collect();
    (cases, bad)
}

/// Built-in labeled + adversarial fixtures for `site`.
pub fn builtin_cases(site: &str) -> Vec<Case> {
    BUILTIN
        .iter()
        .filter(|(s, _)| *s == site)
        .flat_map(|(_, text)| parse_cases(text).0)
        .collect()
}

#[derive(Debug, Serialize)]
pub struct FailureResult {
    pub mode: &'static str,
    pub cases: usize,
    pub fail_open: usize,
    pub fail_open_rate: Option<f64>,
    /// Mean time before falling back to the baseline.
    pub mean_fallback_ms: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct SiteReport {
    pub site: String,
    /// "main" or "adversarial".
    pub set: &'static str,
    pub cases: usize,
    pub replayed: usize,
    pub labeled: usize,
    pub agreement: Option<f64>,
    pub jev: Prf,
    pub baseline: Prf,
    pub latency_cold: Pct,
    pub latency_warm: Pct,
    pub tokens_added: u64,
    pub tokens_avoided: u64,
    pub usd_per_1k: Option<f64>,
    pub failure_modes: Vec<FailureResult>,
    pub overhead_pct: Option<f64>,
    pub verdict: Verdict,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub schema: u32,
    pub backend: String,
    /// Simulated backend: the numbers describe the harness, not the sites.
    pub synthetic: bool,
    pub sites: Vec<SiteReport>,
}

pub const REPORT_SCHEMA: u32 = 1;

/// Decision the site would act on: Jev's answer when it answered with enough
/// confidence, else the baseline (fail open). Second value: Jev was applied.
fn finalize(reply: &Result<Reply, Failure>, case: &Case, min_conf: f64) -> (String, bool) {
    match reply {
        Ok(r) if r.confidence.is_some_and(|c| c >= min_conf) => (r.label.clone(), true),
        _ => (case.baseline.clone(), false),
    }
}

fn replay_set(
    site: &str,
    set: &'static str,
    cases: &[&Case],
    backend: &mut dyn Backend,
    th: &SiteThresholds,
) -> SiteReport {
    let min_conf = th.min_confidence();
    let cold_calls = backend.cold_calls();
    let started = Instant::now();
    let mut backend_wall = std::time::Duration::ZERO;
    backend.force(None);
    let (mut cold, mut warm) = (vec![], vec![]);
    let (mut agree, mut replayed) = (0usize, 0usize);
    let (mut added, mut avoided, mut cost, mut latency_sum) = (0u64, 0u64, 0f64, 0u64);
    let (mut jev_pairs, mut base_pairs) = (vec![], vec![]);
    for (nth, case) in cases.iter().enumerate() {
        let t = Instant::now();
        let reply = backend.ask(case, nth);
        backend_wall += t.elapsed();
        let latency = match &reply {
            Ok(r) => r.latency_us,
            Err(f) => f.latency_us,
        };
        latency_sum += latency;
        (if nth < cold_calls {
            &mut cold
        } else {
            &mut warm
        })
        .push(latency);
        replayed += 1;
        if let Ok(r) = &reply {
            cost += r.cost;
            added += count_tokens(&case.features.to_string()) + count_tokens(&r.label);
        }
        let (fin, applied) = finalize(&reply, case, min_conf);
        if applied {
            avoided += th.llm_tokens_avoided();
        }
        agree += usize::from(fin == case.baseline);
        if let Some(e) = &case.expected {
            jev_pairs.push((fin, e.clone()));
            base_pairs.push((case.baseline.clone(), e.clone()));
        }
    }
    let failure_modes = FailureMode::ALL
        .iter()
        .map(|&mode| failure_pass(mode, cases, backend, min_conf))
        .collect();
    backend.force(None);
    let overhead_us = started.elapsed().saturating_sub(backend_wall).as_micros() as f64;
    let mut report = SiteReport {
        site: site.to_string(),
        set,
        cases: cases.len(),
        replayed,
        labeled: jev_pairs.len(),
        agreement: (replayed > 0).then(|| agree as f64 / replayed as f64),
        jev: Prf::of(&jev_pairs),
        baseline: Prf::of(&base_pairs),
        latency_cold: Pct::of(&cold),
        latency_warm: Pct::of(&warm),
        tokens_added: added,
        tokens_avoided: avoided,
        usd_per_1k: (replayed > 0).then(|| cost / replayed as f64 * 1000.0),
        failure_modes,
        overhead_pct: (latency_sum > 0).then(|| overhead_us / latency_sum as f64 * 100.0),
        verdict: Verdict::default(),
    };
    report.verdict = th.judge(&report);
    report
}

fn failure_pass(
    mode: FailureMode,
    cases: &[&Case],
    backend: &mut dyn Backend,
    min_conf: f64,
) -> FailureResult {
    backend.force(Some(mode));
    let (mut open, mut ms) = (0usize, 0f64);
    for (nth, case) in cases.iter().enumerate() {
        let reply = backend.ask(case, nth);
        let (fin, applied) = finalize(&reply, case, min_conf);
        open += usize::from(!applied && fin == case.baseline);
        ms += match &reply {
            Ok(r) => r.latency_us,
            Err(f) => f.latency_us,
        } as f64
            / 1000.0;
    }
    let n = cases.len();
    FailureResult {
        mode: mode.name(),
        cases: n,
        fail_open: open,
        fail_open_rate: (n > 0).then(|| open as f64 / n as f64),
        mean_fallback_ms: (n > 0).then(|| ms / n as f64),
    }
}

/// Replay `cases` (mixed sites are fine; only `site` rows are used) and
/// report the main and adversarial sets separately.
pub fn run_site(
    site: &str,
    cases: &[Case],
    backend: &mut dyn Backend,
    thresholds: &Thresholds,
) -> Vec<SiteReport> {
    let th = thresholds.for_site(site);
    [("main", false), ("adversarial", true)]
        .into_iter()
        .map(|(set, adv)| {
            let v: Vec<&Case> = cases
                .iter()
                .filter(|c| c.site == site && c.adversarial == adv)
                .collect();
            (set, v)
        })
        .filter(|(set, v)| !v.is_empty() || *set == "main")
        .map(|(set, v)| replay_set(site, set, &v, backend, &th))
        .collect()
}
