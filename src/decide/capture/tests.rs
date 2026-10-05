use super::*;
use serde_json::json;

fn input<'a>(site: &'a str, features: Value, norm: &'a str, summary: &str) -> Input<'a> {
    Input {
        site,
        features,
        norm_input: norm,
        label: json!({ "summary": summary }),
        confidence: Some(0.9),
        baseline: "none",
        source_model: Some("jev-1.13"),
    }
}

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("af-capture-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir.join("dataset.jsonl")
}

#[test]
fn off_by_default() {
    assert!(!enabled_for(None));
    assert!(!enabled_for(Some("0")));
    assert!(!enabled_for(Some("true")));
    assert!(enabled_for(Some("1")));
}

#[test]
fn secret_straddling_clip_boundary_is_redacted() {
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz012345";
    let prompt = format!("{}{secret} tail", "a ".repeat(248)); // cut lands inside the token
    let f = router_features(&prompt);
    assert!(!f["prompt"].as_str().unwrap().contains("ghp_"));
    let state = json!({"mode": "m", "latest_role_reply": format!("{}{secret}", "b".repeat(195))});
    let (jf, norm) = judge_features(&state);
    assert!(!jf["reply"].as_str().unwrap().contains("ghp_"));
    assert!(!norm.contains("ghp_"));
}

#[test]
fn judge_norm_key_separates_plan_len_and_task() {
    let mk = |plan: usize, task: &str| {
        judge_features(&json!({
            "mode": "m", "plan": vec!["x"; plan],
            "current_task": {"title": task}, "latest_role_reply": "r",
        }))
        .1
    };
    assert_ne!(mk(1, "t"), mk(2, "t"));
    assert_ne!(mk(1, "a"), mk(1, "b"));
}

#[test]
fn rerank_candidates_capped() {
    let c: Vec<(&str, &str, f64)> = (0..50).map(|_| ("s", "d", 1.0)).collect();
    let f = rerank_features("p", c);
    assert_eq!(f["candidates"].as_array().unwrap().len(), 10);
}

#[test]
fn rows_round_trip() {
    let p = tmp("rt");
    let row = build_row(input(
        "router",
        router_features("fix the bug?"),
        "fix",
        "easy",
    ));
    append(&p, &row, MAX_BYTES).unwrap();
    append(&p, &row, MAX_BYTES).unwrap();
    let rows = load(&p);
    assert_eq!(rows, vec![row.clone(), row]);
    assert_eq!(rows[0].schema, SCHEMA);
    assert_eq!(rows[0].source_model.as_deref(), Some("jev-1.13"));
}

#[test]
fn normalization_is_stable() {
    let a = "Fix  Bug #123 in  550e8400-e29b-41d4-a716-446655440000 \n now";
    let b = "fix bug # in now";
    assert_eq!(normalize(a), normalize(b));
    assert_eq!(norm_key(a), norm_key("FIX BUG #9 in deadbeef1234   now"));
    assert_ne!(norm_key("fix bug"), norm_key("add feature"));
    // A hex-looking word with no digit is kept.
    assert_eq!(normalize("Defaced"), "defaced");
}

#[test]
fn redaction_applied() {
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz012345";
    let row = build_row(input(
        "router",
        router_features(&format!("use token {secret} please")),
        &format!("use token {secret}"),
        "easy",
    ));
    assert!(!row.features.to_string().contains(secret));
    assert!(row.features.to_string().contains("[REDACTED]"));
}

#[test]
fn rotation_cap() {
    let p = tmp("rot");
    let row = build_row(input("router", json!({"a": 1}), "x", "easy"));
    for _ in 0..20 {
        append(&p, &row, 500).unwrap();
    }
    assert!(rotated(&p).exists());
    assert!(std::fs::metadata(&p).unwrap().len() <= 500 + 400);
    assert!(load(&p).len() >= 2);
}

#[test]
fn clear_removes_files() {
    let p = tmp("clr");
    let row = build_row(input("router", json!({}), "x", "easy"));
    for _ in 0..5 {
        append(&p, &row, 100).unwrap();
    }
    assert_eq!(clear(&p), 2);
    assert!(load(&p).is_empty());
    assert_eq!(clear(&p), 0);
}

#[test]
fn features_bounded() {
    let long = "a".repeat(10_000);
    let f = router_features(&long);
    assert!(f["prompt"].as_str().unwrap().len() <= 500);
    let big: Vec<(&str, &str, f64)> = (0..200).map(|_| ("skill", long.as_str(), 1.0)).collect();
    let row = build_row(input("skill_rerank", rerank_features(&long, big), "x", "a"));
    assert!(row.features.to_string().len() <= norm::MAX_FEATURES_BYTES);
    let state =
        json!({"mode": "implement", "latest_role_reply": long, "plan": [], "ledger_tail": []});
    let (jf, _) = judge_features(&state);
    assert!(jf["reply"].as_str().unwrap().len() <= 500);
}

#[test]
fn judge_markers_and_counts() {
    let state = json!({
        "mode": "review_only",
        "plan": ["0. a", "1. b"],
        "ledger_tail": ["x"],
        "current_task": {"title": "t"},
        "latest_role_reply": "LGTM, all tests pass",
    });
    let (f, norm) = judge_features(&state);
    assert_eq!(f["markers"]["approved"], true);
    assert_eq!(f["markers"]["issues"], false);
    assert_eq!(f["plan_len"], 2);
    assert!(norm.starts_with("review_only|"));
    let rf = router_features("see src/a.rs and ```x``` ok? yes?");
    assert_eq!(
        (
            rf["code_fences"].as_u64(),
            rf["paths"].as_u64(),
            rf["question_marks"].as_u64()
        ),
        (Some(1), Some(1), Some(2))
    );
}

#[test]
fn repetition_rate_computed() {
    let mk = |site: &str, key: &str, label: &str| Row {
        schema: SCHEMA,
        ts: 100,
        site: site.into(),
        features: json!({}),
        norm_key: key.into(),
        label: json!({ "summary": label }),
        confidence: None,
        baseline: "b".into(),
        source_model: None,
    };
    let rows = vec![
        mk("sdd_judge", "k1", "advance_task"),
        mk("sdd_judge", "k1", "advance_task"),
        mk("sdd_judge", "k1", "fix_round"),
        mk("sdd_judge", "k2", "advance_task"),
        mk("router", "a", "easy"),
        mk("router", "b", "hard"),
    ];
    let stats = summarize(&rows);
    let judge = stats.iter().find(|s| s.site == "sdd_judge").unwrap();
    assert_eq!(judge.rows, 4);
    assert_eq!(judge.distinct_keys, 2);
    assert!((judge.repetition - 0.5).abs() < 1e-9); // 2 of 4 seen before
    assert!((judge.top_k_coverage - 1.0).abs() < 1e-9);
    assert_eq!(judge.labels["advance_task"], 3);
    let router = stats.iter().find(|s| s.site == "router").unwrap();
    assert_eq!(router.repetition, 0.0);
    let text = render(&stats);
    assert!(text.contains("sdd_judge: 4 rows") && text.contains("repetition: 50.0%"));
}
