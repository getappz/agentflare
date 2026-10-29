use super::shadow::{self, Row};
use super::{
    report, storage,
    tracking::{self, GroundTruth, OutcomeKind},
};

#[test]
fn decisions_have_unique_join_keys_and_old_rows_still_load() {
    let a = Row::answered("router", "private input", "a", "b", Some(0.9), 12, None);
    let b = Row::failed("router", "private input", "a", "timeout");
    let a = serde_json::to_value(a).unwrap();
    let b = serde_json::to_value(b).unwrap();
    assert!(a["decision_id"].is_string());
    assert_ne!(a["decision_id"], b["decision_id"]);
    let old: Row = serde_json::from_value(serde_json::json!({
        "ts": 1, "site": "old", "input_sha": "abc", "input_len": 3, "baseline": "a"
    }))
    .unwrap();
    assert_eq!(old.site, "old");
}

#[test]
fn report_replay_loads_rotated_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shadow.jsonl");
    let row = Row::answered("site", "private input", "a", "b", Some(0.9), 12, None);
    std::fs::write(
        path.with_extension("jsonl.1"),
        format!("{}\n", serde_json::to_string(&row).unwrap()),
    )
    .unwrap();
    assert_eq!(shadow::load(&path).len(), 1);
}

#[test]
fn raw_record_replay_matches_report_and_calibrates_against_outcomes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shadow.jsonl");
    for i in 0..10 {
        let mut row = Row::answered(
            "router",
            "private prompt",
            "a",
            "b",
            Some(0.9),
            10 + i,
            Some(0.001),
        );
        row.ts = if i < 5 { 1_700_000_000 } else { 1_700_100_000 };
        row.details.source_model = Some(if i < 5 { "jev-v1" } else { "jev-v2" }.into());
        row.details.input_tokens = 3;
        row.details.output_tokens = 2;
        row.details.applied = true;
        storage::append(&path, &row, 5_000_000).unwrap();
        let id = row.decision_id.unwrap();
        if i == 0 {
            storage::append(
                &path,
                &GroundTruth {
                    record_type: "outcome".into(),
                    decision_id: id.clone(),
                    ts: row.ts,
                    kind: OutcomeKind::HumanOverride,
                    answer: "a".into(),
                },
                5_000_000,
            )
            .unwrap();
        }
        tracking::outcome_add(
            &path,
            &id,
            OutcomeKind::PrReviewResult,
            if i < 9 { "b" } else { "a" },
        )
        .unwrap();
    }
    storage::append(
        &path,
        &Row::failed("judge", "input", "a", "timeout"),
        5_000_000,
    )
    .unwrap();
    let r = report::load(&path, &dir.path().join("dataset.jsonl"), None);
    let raw: Vec<serde_json::Value> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let decisions: Vec<_> = raw.iter().filter(|v| v.get("site").is_some()).collect();
    let answered: Vec<_> = decisions.iter().filter(|v| v["jev"].is_string()).collect();
    let agreements = answered
        .iter()
        .filter(|v| v["jev"] == v["baseline"])
        .count();
    assert_eq!(r.all.decisions, decisions.len());
    assert_eq!(r.all.answered, answered.len());
    assert_eq!(r.all.agreements, agreements);
    assert_eq!(
        (r.all.labeled, r.all.jev_right, r.all.baseline_right),
        (10, 9, 1)
    );
    assert_eq!((r.all.input_tokens, r.all.output_tokens), (30, 20));
    assert!((r.all.cost - 0.01).abs() < 1e-10);
    assert_eq!(r.all.overrides, 1);
    assert_eq!(r.all.calibration["0.9-1.0"].accuracy, 0.9);
    assert_eq!(r.models["router"].len(), 2);
    assert_eq!(r.days.len(), 3); // two fixture days + the failure's current date
    assert_eq!(r.disagreements.len(), 10);
    assert_eq!(r.disagreements[0].jev_right, Some(true));
    assert_eq!(r.disagreements[9].baseline_right, Some(true));
    assert_eq!(
        report::load(&path, &dir.path().join("dataset.jsonl"), Some("router"))
            .all
            .decisions,
        10
    );
    assert!(serde_json::to_value(&r).unwrap()["all"]["calibration"].is_object());
}

#[test]
fn manual_outcomes_validate_id_and_clear_covers_both_segments() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shadow.jsonl");
    assert!(tracking::outcome_add(&path, "missing", OutcomeKind::HumanOverride, "a").is_err());
    let row = Row::answered("s", "p", "a", "b", Some(1.0), 1, None);
    storage::append(&path, &row, 1000).unwrap();
    let id = row.decision_id.unwrap();
    tracking::outcome_add(&path, &id, OutcomeKind::IncidentConfirmed, "neither").unwrap();
    let r = report::load(&path, &path.with_file_name("dataset.jsonl"), None);
    assert_eq!(r.disagreements[0].jev_right, Some(false));
    assert_eq!(r.disagreements[0].baseline_right, Some(false));
    super::capture::clear(&path).unwrap();
    assert!(shadow::load(&path).is_empty());
    assert!(storage::load::<GroundTruth>(&path).is_empty());
}

#[test]
fn outcome_add_waits_for_a_row_another_writer_is_still_appending() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shadow.jsonl");
    let row = Row::answered("s", "p", "a", "b", Some(1.0), 1, None);
    let id = row.decision_id.clone().unwrap();
    let late = path.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(250));
        storage::append(&late, &row, 1000).unwrap();
    });
    tracking::outcome_add(&path, &id, OutcomeKind::IncidentConfirmed, "neither").unwrap();
    writer.join().unwrap();
}

#[test]
fn batch_usage_is_allocated_once_including_integer_remainders() {
    use super::{Answer, Outcome, Response, Usage};
    use std::collections::BTreeMap;
    let out = Outcome {
        elapsed_ms: 7,
        response: Response {
            answers: BTreeMap::from([
                ("router.q".into(), Answer::Noul { noul: 0.9 }),
                ("skill.a".into(), Answer::Noul { noul: 0.1 }),
                ("skill.b".into(), Answer::Noul { noul: 0.1 }),
            ]),
            model: Some("v1".into()),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 2,
                cost: Some(0.3),
            },
        },
    };
    let a = out.for_prefix("router.");
    let b = out.for_prefix("skill.");
    assert_eq!(
        a.response.usage.input_tokens + b.response.usage.input_tokens,
        10
    );
    assert_eq!(
        a.response.usage.output_tokens + b.response.usage.output_tokens,
        2
    );
    assert!((a.response.usage.cost.unwrap() + b.response.usage.cost.unwrap() - 0.3).abs() < 1e-10);
    assert_eq!(b.response.answers.len(), 2);
}

#[test]
fn statuses_distinguish_fail_open_reasons() {
    use super::DecideError;
    for (e, expected) in [
        (DecideError::Unavailable, "breaker-open"),
        (DecideError::Status(402, String::new()), "402"),
        (DecideError::Transport("timed out".into()), "timeout"),
        (DecideError::Transport("request TIMEOUT".into()), "timeout"),
        (
            DecideError::Transport("runtime failure".into()),
            "fail-open",
        ),
        (
            DecideError::Transport("estimate unavailable".into()),
            "fail-open",
        ),
        (DecideError::Malformed("bad".into()), "fail-open"),
    ] {
        assert_eq!(tracking::status(&e), expected);
    }
}

#[test]
fn failed_envelopes_only_classify_explicit_timeout_messages() {
    for (message, expected) in [
        ("runtime failure", "fail-open"),
        ("estimate unavailable", "fail-open"),
        ("request TIMED OUT", "timeout"),
        ("request TIMEOUT", "timeout"),
    ] {
        assert_eq!(
            Row::failed("s", "p", "a", message)
                .details
                .status
                .as_deref(),
            Some(expected)
        );
    }
}

#[test]
fn breaker_open_rows_do_not_change_agreement_or_failure_rates() {
    let mut rows = vec![
        Row::answered("router", "p", "a", "a", None, 10, None),
        Row::answered("router", "p", "a", "b", None, 20, None),
        Row::failed("router", "p", "a", "timeout"),
    ];
    rows.extend((0..100).map(|_| Row::failed("router", "p", "a", "circuit breaker open")));
    let r = report::replay(&rows, &[], None);
    for metrics in std::iter::once(&r.all)
        .chain(r.sites.values())
        .chain(r.models.values().flat_map(|models| models.values()))
        .chain(r.days.values())
    {
        let json = serde_json::to_value(metrics).unwrap();
        assert_eq!(metrics.decisions, 103);
        assert_eq!(metrics.agreement_rate, Some(0.5));
        assert_eq!(json["failed"], 1);
        assert_eq!(json["breaker_open"], 100);
        assert_eq!(json["failure_rate"], serde_json::json!(1.0 / 3.0));
    }
    assert!(report::render(&r).contains("breaker-open 100"));
    let only_breakers = report::replay(&rows[3..], &[], None);
    let json = serde_json::to_value(&only_breakers.all).unwrap();
    assert!(json["failure_rate"].is_null());
    assert!(only_breakers.all.agreement_rate.is_none());
}

#[test]
fn typed_comparison_ignores_json_whitespace() {
    assert!(tracking::equivalent("{\"score\":1}", "{\"score\":1.0}"));
    assert!(!tracking::equivalent("{\"score\":1}", "{\"score\":1.1}"));
    let row = Row::answered(
        "new-site",
        "private",
        "{\"q\": \"a\"}",
        "{\"q\":\"a\"}",
        Some(0.9),
        1,
        None,
    );
    let outcome = GroundTruth {
        record_type: "outcome".into(),
        decision_id: row.decision_id.clone().unwrap(),
        ts: row.ts,
        kind: OutcomeKind::PrReviewResult,
        answer: "{ \"q\": \"a\" }".into(),
    };
    let r = report::replay(&[row], &[outcome], None);
    assert_eq!(r.all.agreements, 1);
    assert_eq!(r.all.jev_right, 1);
    assert_eq!(r.all.baseline_right, 1);
    assert!(r.disagreements.is_empty());
}

#[test]
fn recording_redacts_summaries_and_preserves_only_question_types() {
    use super::{Answer, Question, Usage};
    use std::{cell::RefCell, collections::BTreeMap};
    let questions = BTreeMap::from([("q".into(), Question::noul("private question text"))]);
    let answers = BTreeMap::from([("q".into(), Answer::Noul { noul: 0.9 })]);
    let rows = RefCell::new(Vec::new());
    let secret = format!("ghp_{}", "a".repeat(36));
    let id = tracking::finish(
        Row::answered(
            "site",
            "private input",
            &secret,
            &secret,
            Some(0.9),
            2,
            None,
        ),
        tracking::Context {
            questions: &questions,
            answers: &answers,
            source_model: Some("v1"),
            usage: &Usage::default(),
            applied: true,
        },
        None,
        &|r| rows.borrow_mut().push(r.clone()),
    );
    let rows = rows.borrow();
    let text = serde_json::to_string(&rows[0]).unwrap();
    assert!(!text.contains(&secret));
    assert!(!text.contains("private"));
    assert_eq!(rows[0].decision_id.as_deref(), Some(id.as_str()));
    assert_eq!(rows[0].details.question_types["q"], "noul");
    assert!(rows[0].details.applied);
}

#[test]
fn generic_summary_uses_decisions_instead_of_confidence_metadata() {
    use super::Answer;
    use std::collections::BTreeMap;
    let choice = |confidence| Answer::Choice {
        choice: "a".into(),
        confidence: Some(confidence),
        probabilities: BTreeMap::new(),
    };
    let a = BTreeMap::from([
        ("q".into(), choice(0.8)),
        ("yes".into(), Answer::Noul { noul: 0.9 }),
    ]);
    let b = BTreeMap::from([
        ("q".into(), choice(0.95)),
        ("yes".into(), Answer::Noul { noul: 0.7 }),
    ]);
    assert_eq!(tracking::summary(&a), tracking::summary(&b));
    assert_eq!(tracking::summary(&a), "{\"q\":\"a\",\"yes\":true}");
}

#[test]
fn disabled_helper_preserves_result_without_allocating_id() {
    let tracked = tracking::ask_with(
        "test",
        "none",
        |_| false,
        &serde_json::Value::Null,
        &Default::default(),
        || Err(super::DecideError::Disabled),
    );
    assert!(tracked.decision_id.is_none());
    assert!(matches!(tracked.result, Err(super::DecideError::Disabled)));
}

#[test]
fn capture_supplement_joins_the_envelope_and_redacts_questions() {
    use super::{Answer, Question};
    use std::collections::BTreeMap;
    let secret = format!("ghp_{}", "b".repeat(36));
    let questions = BTreeMap::from([(
        "q".into(),
        Question::noul(&format!("private instructions {secret}")),
    )]);
    let answers = BTreeMap::from([("q".into(), Answer::Noul { noul: 0.8 })]);
    let envelope = Row::answered("s", "private input", "false", "true", None, 1, None);
    let row = tracking::captured(
        envelope.decision_id.as_deref().unwrap(),
        super::capture::Input {
            site: "s",
            features: serde_json::json!({"prompt": secret}),
            norm_input: "private input",
            label: serde_json::json!({"summary": "true"}),
            confidence: None,
            baseline: "false",
            source_model: None,
        },
        &questions,
        &answers,
    );
    assert_eq!(row.decision_id, envelope.decision_id);
    let serialized = serde_json::to_string(&row).unwrap();
    assert!(!serialized.contains(&secret));
    assert!(serialized.contains("private instructions"));
    assert!(serialized.contains("REDACTED"));
    let mut old = serde_json::to_value(row).unwrap();
    for key in ["decision_id", "question", "answer"] {
        old.as_object_mut().unwrap().remove(key);
    }
    assert!(
        serde_json::from_value::<super::capture::Row>(old)
            .unwrap()
            .decision_id
            .is_none()
    );
}

#[test]
fn report_excludes_unknown_outcomes_and_retains_model_and_failure_trends() {
    let mut known = Row::answered("a", "p", "yes", "yes", Some(0.9), 5, Some(0.01));
    known.details.source_model = Some("v1".into());
    let mut unknown = Row::answered("a", "p", "no", "yes", Some(0.9), 8, Some(0.02));
    unknown.details.source_model = Some("v2".into());
    let failed = Row::failed("b", "p", "no", "402");
    let outcomes = vec![GroundTruth {
        record_type: "outcome".into(),
        decision_id: known.decision_id.clone().unwrap(),
        ts: known.ts,
        kind: OutcomeKind::PrReviewResult,
        answer: "yes".into(),
    }];
    let r = report::replay(&[known, unknown, failed], &outcomes, None);
    assert_eq!(
        (
            r.all.decisions,
            r.all.answered,
            r.all.agreements,
            r.all.labeled
        ),
        (3, 2, 1, 1)
    );
    assert_eq!(r.all.calibration["0.9-1.0"].accuracy, 1.0);
    assert_eq!(r.all.statuses["402"], 1);
    assert_eq!(r.models["a"]["v2"].labeled, 0);
    assert!(r.disagreements[0].outcome.is_none());
}

#[test]
fn missing_baselines_do_not_change_comparison_rates_or_ground_truth_accuracy() {
    let rows = vec![
        Row::answered("router", "p", "a", "a", Some(0.9), 1, None),
        Row::answered("router", "p", "a", "b", Some(0.9), 1, None),
        Row::answered("router", "p", "none", "none", Some(0.9), 1, None),
        Row::answered(
            "model_routing",
            "p",
            "native-default",
            "b",
            Some(0.9),
            1,
            None,
        ),
        Row::answered("review_triage", "p", "unjudged", "b", Some(0.9), 1, None),
        Row::answered("ping", "p", "none", "b", Some(0.9), 1, None),
    ];
    let outcomes: Vec<_> = rows
        .iter()
        .map(|r| GroundTruth {
            record_type: "outcome".into(),
            decision_id: r.decision_id.clone().unwrap(),
            ts: r.ts,
            kind: OutcomeKind::PrReviewResult,
            answer: r.jev.clone().unwrap(),
        })
        .collect();
    let r = report::replay(&rows, &outcomes, None);
    let metrics = serde_json::to_value(&r.all).unwrap();
    assert_eq!(
        (
            r.all.decisions,
            r.all.answered,
            r.all.labeled,
            r.all.jev_right
        ),
        (6, 6, 6, 6)
    );
    assert_eq!(r.all.agreements, 2);
    assert_eq!(metrics["compared"], 3);
    assert_eq!(metrics["baseline_labeled"], 3);
    assert_eq!(r.all.baseline_right, 2);
    assert_eq!(r.all.agreement_rate, Some(2.0 / 3.0));
    assert_eq!(r.disagreements.len(), 1);
    for site in ["model_routing", "review_triage", "ping"] {
        assert_eq!(r.sites[site].answered, 1);
        assert_eq!(r.sites[site].jev_right, 1);
        assert!(r.sites[site].agreement_rate.is_none());
        assert_eq!(
            serde_json::to_value(&r.sites[site]).unwrap()["baseline_labeled"],
            0
        );
    }
}
