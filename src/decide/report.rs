//! Deterministic replay over the two retained ledger segments; no cached scores.
use super::tracking::equivalent;
use super::{
    shadow, storage,
    tracking::{GroundTruth, OutcomeKind},
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Default, Serialize, PartialEq)]
pub struct Metrics {
    pub decisions: usize,
    pub breaker_open: usize,
    pub failed: usize,
    pub failure_rate: Option<f64>,
    pub answered: usize,
    pub compared: usize,
    pub agreements: usize,
    pub applied: usize,
    pub labeled: usize,
    pub jev_right: usize,
    pub baseline_right: usize,
    pub baseline_labeled: usize,
    pub overrides: usize,
    pub override_rate: Option<f64>,
    pub agreement_rate: Option<f64>,
    pub statuses: BTreeMap<String, usize>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost: f64,
    pub median_ms: Option<u64>,
    pub p95_ms: Option<u64>,
    pub calibration: BTreeMap<String, Calibration>,
}

#[derive(Debug, Default, Serialize, PartialEq)]
pub struct Calibration {
    pub labeled: usize,
    pub correct: usize,
    pub mean_confidence: f64,
    pub accuracy: f64,
}

#[derive(Debug, Serialize)]
pub struct Disagreement {
    pub decision_id: Option<String>,
    pub site: String,
    pub ts: i64,
    pub baseline: String,
    pub jev: String,
    pub outcome: Option<String>,
    pub outcome_kind: Option<OutcomeKind>,
    pub jev_right: Option<bool>,
    pub baseline_right: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub schema: u32,
    pub all: Metrics,
    pub sites: BTreeMap<String, Metrics>,
    /// Site -> model version -> metrics. Missing historical versions stay unknown.
    pub models: BTreeMap<String, BTreeMap<String, Metrics>>,
    /// UTC date -> metrics, in chronological order.
    pub days: BTreeMap<String, Metrics>,
    pub disagreements: Vec<Disagreement>,
    pub dropped_writes: u64,
    pub ledger_bytes: u64,
    pub capture_bytes: u64,
    pub bytes_per_1000_decisions: Option<f64>,
    pub orphan_outcomes: usize,
}

fn has_baseline(row: &shadow::Row) -> bool {
    !matches!(
        (row.site.as_str(), row.baseline.as_str()),
        ("model_routing", "native-default") | ("review_triage", "unjudged") | ("ping", "none")
    )
}

pub fn replay(rows: &[shadow::Row], outcomes: &[GroundTruth], site: Option<&str>) -> Report {
    let latest: BTreeMap<_, _> = outcomes
        .iter()
        .filter(|o| o.record_type == "outcome")
        .map(|o| (o.decision_id.as_str(), o))
        .collect();
    let overrides: std::collections::BTreeSet<_> = outcomes
        .iter()
        .filter(|o| o.kind == OutcomeKind::HumanOverride)
        .map(|o| o.decision_id.as_str())
        .collect();
    let selected: Vec<_> = rows
        .iter()
        .filter(|r| site.is_none_or(|s| r.site == s))
        .collect();
    let metrics = |rs: &[&shadow::Row]| summarize(rs, &latest, &overrides);
    let mut sites: BTreeMap<String, Vec<&shadow::Row>> = BTreeMap::new();
    let mut models: BTreeMap<String, BTreeMap<String, Vec<&shadow::Row>>> = BTreeMap::new();
    let mut days: BTreeMap<String, Vec<&shadow::Row>> = BTreeMap::new();
    let mut disagreements = Vec::new();
    for r in &selected {
        sites.entry(r.site.clone()).or_default().push(r);
        models
            .entry(r.site.clone())
            .or_default()
            .entry(
                r.details
                    .source_model
                    .clone()
                    .unwrap_or_else(|| "unknown".into()),
            )
            .or_default()
            .push(r);
        let day = chrono::DateTime::from_timestamp(r.ts, 0)
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "unknown".into());
        days.entry(day).or_default().push(r);
        if let Some(jev) = r
            .jev
            .as_ref()
            .filter(|j| r.error.is_none() && has_baseline(r) && !equivalent(j, &r.baseline))
        {
            let outcome = r.decision_id.as_deref().and_then(|id| latest.get(id));
            disagreements.push(Disagreement {
                decision_id: r.decision_id.clone(),
                site: r.site.clone(),
                ts: r.ts,
                baseline: r.baseline.clone(),
                jev: jev.clone(),
                outcome: outcome.map(|o| o.answer.clone()),
                outcome_kind: outcome.map(|o| o.kind.clone()),
                jev_right: outcome.map(|o| equivalent(&o.answer, jev)),
                baseline_right: outcome.map(|o| equivalent(&o.answer, &r.baseline)),
            });
        }
    }
    Report {
        schema: 1,
        all: metrics(&selected),
        sites: sites.into_iter().map(|(k, rs)| (k, metrics(&rs))).collect(),
        models: models
            .into_iter()
            .map(|(k, groups)| {
                (
                    k,
                    groups
                        .into_iter()
                        .map(|(m, rs)| (m, metrics(&rs)))
                        .collect(),
                )
            })
            .collect(),
        days: days.into_iter().map(|(k, rs)| (k, metrics(&rs))).collect(),
        disagreements,
        dropped_writes: 0,
        ledger_bytes: 0,
        capture_bytes: 0,
        bytes_per_1000_decisions: None,
        orphan_outcomes: latest
            .keys()
            .filter(|id| !rows.iter().any(|r| r.decision_id.as_deref() == Some(**id)))
            .count(),
    }
}

fn summarize(
    rows: &[&shadow::Row],
    outcomes: &BTreeMap<&str, &GroundTruth>,
    overrides: &std::collections::BTreeSet<&str>,
) -> Metrics {
    let mut s = Metrics {
        decisions: rows.len(),
        ..Default::default()
    };
    let mut latency = Vec::new();
    for r in rows {
        let status = r
            .details
            .status
            .clone()
            .unwrap_or_else(|| if r.error.is_some() { "fail-open" } else { "ok" }.into());
        *s.statuses.entry(status.clone()).or_default() += 1;
        if status == "breaker-open" {
            s.breaker_open += 1;
            continue;
        }
        s.applied += usize::from(r.details.applied);
        s.cost += r.cost.unwrap_or_default();
        s.input_tokens += r.details.input_tokens;
        s.output_tokens += r.details.output_tokens;
        latency.extend(r.latency_ms);
        let outcome = r.decision_id.as_deref().and_then(|id| outcomes.get(id));
        s.overrides += usize::from(
            r.decision_id
                .as_deref()
                .is_some_and(|id| overrides.contains(id)),
        );
        let Some(jev) = r.jev.as_ref().filter(|_| r.error.is_none()) else {
            s.failed += 1;
            continue;
        };
        s.answered += 1;
        if has_baseline(r) {
            s.compared += 1;
            s.agreements += usize::from(equivalent(jev, &r.baseline));
        }
        if let Some(o) = outcome {
            let right = equivalent(&o.answer, jev);
            s.labeled += 1;
            s.jev_right += usize::from(right);
            if has_baseline(r) {
                s.baseline_labeled += 1;
                s.baseline_right += usize::from(equivalent(&o.answer, &r.baseline));
            }
            if let Some(c) = r
                .confidence
                .filter(|c| c.is_finite() && (0.0..=1.0).contains(c))
            {
                let bin = ((c * 10.0).floor() as usize).min(9);
                let b = s
                    .calibration
                    .entry(format!(
                        "{:.1}-{:.1}",
                        bin as f64 / 10.0,
                        (bin + 1) as f64 / 10.0
                    ))
                    .or_default();
                b.labeled += 1;
                b.correct += usize::from(right);
                b.mean_confidence += c;
            }
        }
    }
    for b in s.calibration.values_mut() {
        b.mean_confidence /= b.labeled as f64;
        b.accuracy = b.correct as f64 / b.labeled as f64;
    }
    latency.sort_unstable();
    s.median_ms = latency.get(latency.len() / 2).copied();
    s.p95_ms = latency
        .get((latency.len() * 95).div_ceil(100).saturating_sub(1))
        .copied();
    s.agreement_rate = (s.compared > 0).then(|| s.agreements as f64 / s.compared as f64);
    let attempted = s.decisions - s.breaker_open;
    s.failure_rate = (attempted > 0).then(|| s.failed as f64 / attempted as f64);
    s.override_rate = (attempted > 0).then(|| s.overrides as f64 / attempted as f64);
    s
}

pub fn load(path: &Path, capture_path: &Path, site: Option<&str>) -> Report {
    let rows = shadow::load(path);
    let outcomes = storage::load(path);
    let mut report = replay(&rows, &outcomes, site);
    let events: Vec<serde_json::Value> = storage::load(path);
    let decisions: std::collections::BTreeSet<_> = rows
        .iter()
        .filter_map(|r| r.decision_id.as_deref())
        .collect();
    let captured: Vec<super::capture::Row> = storage::load(capture_path);
    let captures: std::collections::BTreeSet<_> = captured
        .iter()
        .filter_map(|r| r.decision_id.as_deref())
        .collect();
    let (mut pending_decisions, mut pending_captures) = (
        std::collections::BTreeSet::new(),
        std::collections::BTreeSet::new(),
    );
    // A timed-out append can finish late; count each missing identified write once.
    for event in events.iter().filter(|v| v["record_type"] == "dropped") {
        let ids = |name: &str| -> Vec<String> {
            event[name]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        };
        let decision_ids = ids("decision_ids");
        let capture_ids = ids("capture_ids");
        report.dropped_writes += event["count"]
            .as_u64()
            .unwrap_or_default()
            .saturating_sub((decision_ids.len() + capture_ids.len()) as u64);
        pending_decisions.extend(decision_ids);
        pending_captures.extend(capture_ids);
    }
    report.dropped_writes += pending_decisions
        .iter()
        .filter(|id| !decisions.contains(id.as_str()))
        .count() as u64
        + pending_captures
            .iter()
            .filter(|id| !captures.contains(id.as_str()))
            .count() as u64
        + storage::pending_drops();
    report.ledger_bytes = storage::bytes(path);
    report.capture_bytes = storage::bytes(capture_path);
    report.bytes_per_1000_decisions = (!rows.is_empty())
        .then(|| (report.ledger_bytes + report.capture_bytes) as f64 * 1000.0 / rows.len() as f64);
    report
}

pub fn render(report: &Report) -> String {
    let mut text = format!(
        "decisions: {}, dropped writes: {}, ledger: {} bytes, capture: {} bytes, bytes/1k: {:?}\n",
        report.all.decisions,
        report.dropped_writes,
        report.ledger_bytes,
        report.capture_bytes,
        report.bytes_per_1000_decisions
    );
    text.push_str("calibration uses labeled outcomes only; unknown outcomes are excluded\n");
    for (name, s) in std::iter::once(("all sites", &report.all))
        .chain(report.sites.iter().map(|(n, s)| (n.as_str(), s)))
    {
        text.push_str(&format!(
            "{name}: failed {}/{}, breaker-open {}\n",
            s.failed,
            s.decisions - s.breaker_open,
            s.breaker_open
        ));
        text.push_str(&format!("{name}: agreement {}/{}, labeled Jev right {}/{}, baseline right {}/{}, overrides {}/{}, applied {}, latency p50/p95 {:?}/{:?} ms, tokens {}/{}, cost ${:.6}\n", s.agreements, s.compared, s.jev_right, s.labeled, s.baseline_right, s.baseline_labeled, s.overrides, s.decisions - s.breaker_open, s.applied, s.median_ms, s.p95_ms, s.input_tokens, s.output_tokens, s.cost));
        for (bin, b) in &s.calibration {
            text.push_str(&format!(
                "  confidence {bin}: mean {:.3}, accuracy {:.3} ({}/{})\n",
                b.mean_confidence, b.accuracy, b.correct, b.labeled
            ));
        }
    }
    for (site, models) in &report.models {
        for (model, s) in models {
            text.push_str(&format!("model {site}/{model}: {} decisions, agreement {}/{}, right {}/{}, p95 {:?} ms, tokens {}/{}, cost ${:.6}\n", s.decisions, s.agreements, s.compared, s.jev_right, s.labeled, s.p95_ms, s.input_tokens, s.output_tokens, s.cost));
        }
    }
    for (day, s) in &report.days {
        text.push_str(&format!("day {day}: {} decisions, agreement {}/{}, right {}/{}, p95 {:?} ms, tokens {}/{}, cost ${:.6}\n", s.decisions, s.agreements, s.compared, s.jev_right, s.labeled, s.p95_ms, s.input_tokens, s.output_tokens, s.cost));
    }
    for d in &report.disagreements {
        text.push_str(&format!("disagree {} {}: baseline={}, Jev={}, outcome={:?}, Jev right={:?}, baseline right={:?}\n", d.decision_id.as_deref().unwrap_or("legacy"), d.site, d.baseline, d.jev, d.outcome, d.jev_right, d.baseline_right));
    }
    text
}
