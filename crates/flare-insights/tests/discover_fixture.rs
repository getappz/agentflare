use chrono::{DateTime, Duration, Utc};
use flare_insights::discover::{mine, Destination, DiscoverOptions, DEFAULT_JACCARD_THRESHOLD};
use flare_insights::*;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Deserialize)]
struct LabelledRow {
    text: String,
    session_id: String,
    expected_group: String,
    expected_kind: String,
}

fn now() -> DateTime<Utc> {
    "2026-02-01T00:00:00Z".parse().unwrap()
}

fn load_rows() -> Vec<LabelledRow> {
    serde_json::from_str(include_str!("fixtures/discover/labelled.json")).unwrap()
}

fn load_store(rows: &[LabelledRow]) -> InsightsStore {
    let store = InsightsStore::open_in_memory().unwrap();
    let updated = now() - Duration::days(1);
    for (i, row) in rows.iter().enumerate() {
        store
            .upsert_session(&Session {
                id: row.session_id.clone(),
                source: SessionSource::ClaudeCode,
                project: "discover-fixture".into(),
                project_path: Some("/tmp/discover-fixture".into()),
                title: None,
                model: None,
                status: SessionStatus::Completed,
                awaiting_reason: None,
                started_at: None,
                updated_at: updated,
                ended_at: None,
                duration_secs: None,
                tokens: TokenUsage {
                    input: 10,
                    output: 10,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                cost: None,
                cost_basis: CostBasis::Unknown,
                pricing_key: None,
                turn_count: 1,
                tool_call_count: 0,
                subagent_count: 0,
                tags: vec![],
                starred: false,
                pid: None,
                cwd: Some("/tmp/discover-fixture".into()),
            })
            .unwrap();
        store
            .upsert_turn(&Turn {
                id: format!("t-{i}"),
                session_id: row.session_id.clone(),
                seq: 1,
                user_text: Some(row.text.clone()),
                assistant_text: None,
                started_at: None,
                ended_at: None,
                tokens: None,
                cost_usd: None,
            })
            .unwrap();
    }
    store
}

fn opts(jaccard: f64) -> DiscoverOptions {
    DiscoverOptions {
        days: 30,
        project_prefix: None,
        min_sessions: 2,
        min_count: 3,
        jaccard_threshold: jaccard,
    }
}

/// Pairwise precision/recall over labelled rows that belong to multi-member gold groups.
fn precision_recall(
    rows: &[LabelledRow],
    report: &flare_insights::evidence::Report<Vec<flare_insights::discover::Proposal>>,
) -> (f64, f64, f64) {
    let multi: HashSet<&str> = {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for row in rows {
            *counts.entry(row.expected_group.as_str()).or_default() += 1;
        }
        counts
            .into_iter()
            .filter(|(_, n)| *n >= 2)
            .map(|(g, _)| g)
            .collect()
    };

    let mut pred: HashMap<&str, String> = HashMap::new();
    for p in &report.body {
        for ex in &p.examples {
            pred.entry(ex.session_id.as_str())
                .or_insert_with(|| p.cluster_id.clone());
        }
    }

    let mut tp = 0usize;
    let mut gold_pairs = 0usize;
    let mut pred_pairs = 0usize;
    for i in 0..rows.len() {
        for j in (i + 1)..rows.len() {
            let same_gold = rows[i].expected_group == rows[j].expected_group
                && multi.contains(rows[i].expected_group.as_str());
            let ci = pred.get(rows[i].session_id.as_str());
            let cj = pred.get(rows[j].session_id.as_str());
            let same_pred = matches!((ci, cj), (Some(a), Some(b)) if a == b);
            if same_gold {
                gold_pairs += 1;
            }
            if same_pred {
                pred_pairs += 1;
                if same_gold {
                    tp += 1;
                }
            }
        }
    }

    let precision = if pred_pairs == 0 {
        0.0
    } else {
        tp as f64 / pred_pairs as f64
    };
    let recall = if gold_pairs == 0 {
        0.0
    } else {
        tp as f64 / gold_pairs as f64
    };
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };
    (precision, recall, f1)
}

#[test]
fn discover_precision_recall_on_fixture() {
    let rows = load_rows();
    assert!(rows.len() >= 40, "fixture must have ≥40 labelled rows");
    let store = load_store(&rows);

    let mut best_t = 0.0;
    let mut best_f1 = -1.0;
    let mut best_pr = (0.0, 0.0);
    println!("jaccard sweep (precision / recall / f1):");
    let mut t = 0.20;
    while t <= 0.90 + 1e-9 {
        let report = mine(&store, &opts(t), now()).unwrap();
        let (p, r, f1) = precision_recall(&rows, &report);
        println!(
            "  jaccard={t:.2}: P={p:.3} R={r:.3} F1={f1:.3} clusters={}",
            report.body.len()
        );
        // On an F1 tie, prefer the highest threshold (stricter merges, still optimal).
        if f1 > best_f1 + 1e-12 || ((f1 - best_f1).abs() < 1e-12 && t > best_t) {
            best_f1 = f1;
            best_t = t;
            best_pr = (p, r);
        }
        t += 0.05;
    }
    println!(
        "chosen jaccard={best_t:.2} (F1={best_f1:.3}, P={:.3}, R={:.3}); DEFAULT_JACCARD_THRESHOLD={DEFAULT_JACCARD_THRESHOLD}",
        best_pr.0, best_pr.1
    );
    assert!(
        (best_t - DEFAULT_JACCARD_THRESHOLD).abs() < 1e-9,
        "update DEFAULT_JACCARD_THRESHOLD to the sweep winner {best_t} (now {DEFAULT_JACCARD_THRESHOLD})"
    );

    let report = mine(&store, &DiscoverOptions::default(), now()).unwrap();
    let (precision, recall, f1) = precision_recall(&rows, &report);
    println!("default: P={precision:.3} R={recall:.3} F1={f1:.3}");
    assert!(precision >= 0.8, "precision {precision} < 0.8");
    assert!(recall >= 0.6, "recall {recall} < 0.6");
}

#[test]
fn rejected_destination_is_none_for_ambiguous_group() {
    let rows = load_rows();
    let store = load_store(&rows);
    let report = mine(&store, &DiscoverOptions::default(), now()).unwrap();

    let ambiguous: Vec<&LabelledRow> = rows
        .iter()
        .filter(|r| r.expected_group == "ambiguous_fix")
        .collect();
    assert!(ambiguous.iter().all(|r| r.expected_kind == "none"));
    let ambiguous_sessions: HashSet<&str> =
        ambiguous.iter().map(|r| r.session_id.as_str()).collect();
    let hit = report.body.iter().find(|p| {
        p.examples
            .iter()
            .any(|e| ambiguous_sessions.contains(e.session_id.as_str()))
    });
    let Some(p) = hit else {
        panic!("expected ambiguous_fix to form a proposal cluster");
    };
    assert!(
        matches!(p.destination, Destination::None),
        "ambiguous group destination was {:?}, want None",
        p.destination
    );
}

#[test]
fn run_twice_is_stable() {
    let rows = load_rows();
    let store = load_store(&rows);
    let a = mine(&store, &DiscoverOptions::default(), now()).unwrap();
    let b = mine(&store, &DiscoverOptions::default(), now()).unwrap();
    let ja = serde_json::to_value(&a.body).unwrap();
    let jb = serde_json::to_value(&b.body).unwrap();
    assert_eq!(ja, jb);
}

fn row(text: String, session_id: String) -> LabelledRow {
    LabelledRow {
        text,
        session_id,
        expected_group: "g".into(),
        expected_kind: "none".into(),
    }
}

#[test]
fn prompt_repeated_thousands_of_times_stays_one_cluster() {
    let rows: Vec<_> = (0..3000)
        .map(|i| {
            row(
                "please run the whole test suite again".into(),
                format!("s{i}"),
            )
        })
        .collect();
    let store = load_store(&rows);
    let report = mine(&store, &opts(DEFAULT_JACCARD_THRESHOLD), now()).unwrap();
    assert_eq!(report.body.len(), 1);
    assert_eq!(report.body[0].occurrences, 3000);
}

#[test]
fn near_duplicates_still_cluster_transitively() {
    let texts = [
        "please run the whole test suite again now",
        "please run the whole test suite again today",
        "please run the whole test suite again now today",
    ];
    let rows: Vec<_> = texts
        .iter()
        .enumerate()
        .map(|(i, t)| row((*t).into(), format!("s{i}")))
        .collect();
    let store = load_store(&rows);
    let report = mine(&store, &opts(DEFAULT_JACCARD_THRESHOLD), now()).unwrap();
    assert_eq!(report.body.len(), 1);
    assert_eq!(report.body[0].occurrences, 3);
}

#[test]
fn coverage_counts_sessions_with_untimed_turns() {
    let rows = load_rows();
    let store = load_store(&rows);
    let report = mine(&store, &opts(DEFAULT_JACCARD_THRESHOLD), now()).unwrap();
    assert_eq!(
        report.coverage.sessions_missing_turn_timestamps, report.coverage.sessions_in_window,
        "fixture turns carry no timestamps"
    );
}

#[test]
fn reordered_prompts_are_not_treated_as_identical() {
    let mut rows = Vec::new();
    for (prefix, text) in [
        ("a", "alpha bravo charlie delta echo foxtrot"),
        ("b", "alpha charlie echo bravo delta foxtrot"),
    ] {
        for i in 1..=3 {
            rows.push(row(text.into(), format!("{prefix}{i}")));
        }
    }
    rows.push(row(
        "alpha charlie echo bravo delta golf".into(),
        "c1".into(),
    ));
    let store = load_store(&rows);
    let report = mine(&store, &opts(DEFAULT_JACCARD_THRESHOLD), now()).unwrap();
    let mut sizes: Vec<_> = report.body.iter().map(|p| p.occurrences).collect();
    sizes.sort_unstable();
    assert_eq!(sizes, [3, 4]);
}

#[test]
fn size_bound_does_not_reject_exact_threshold_jaccard() {
    // 7-token prompt ⊂ 100-token prompt → Jaccard = 7/100 = 0.07.
    // Comparing `min < threshold * max` falsely rejects because 0.07 * 100
    // is 7.000000000000001 in f64; comparing the size ratio does not.
    let small: String = (0..7)
        .map(|i| format!("t{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    let large: String = (0..100)
        .map(|i| format!("t{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    let rows = vec![
        row(small, "s0".into()),
        row(large.clone(), "s1".into()),
        row(large, "s2".into()),
    ];
    let store = load_store(&rows);
    let report = mine(&store, &opts(0.07), now()).unwrap();
    assert_eq!(
        report.body.len(),
        1,
        "pair at the threshold must still cluster"
    );
    assert_eq!(report.body[0].occurrences, 3);
}
