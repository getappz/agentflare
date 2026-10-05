use super::*;
use crate::decide::{Answer, DecideError, Outcome, Response, Usage, shadow};
use std::cell::RefCell;
use std::collections::BTreeMap;

fn task(i: usize, body_len: usize) -> SddTask {
    SddTask {
        id: i,
        title: format!("Task {i}"),
        body: "b".repeat(body_len),
        model_tier: None,
    }
}

/// Exhaustive on purpose: adding a `JudgeAction` stops this compiling until the
/// shadow question (and this list) cover it.
fn key(action: JudgeAction) -> &'static str {
    match action {
        JudgeAction::ContinueTask => "continue_task",
        JudgeAction::FixRound => "fix_round",
        JudgeAction::Escalate => "escalate",
        JudgeAction::ParkFinding => "park_finding",
        JudgeAction::RuleAndContinue => "rule_and_continue",
        JudgeAction::InsertTask => "insert_task",
        JudgeAction::SkipTask => "skip_task",
        JudgeAction::AdvanceTask => "advance_task",
        JudgeAction::CompletePipeline => "complete_pipeline",
    }
}

const ALL: [JudgeAction; 9] = [
    JudgeAction::ContinueTask,
    JudgeAction::FixRound,
    JudgeAction::Escalate,
    JudgeAction::ParkFinding,
    JudgeAction::RuleAndContinue,
    JudgeAction::InsertTask,
    JudgeAction::SkipTask,
    JudgeAction::AdvanceTask,
    JudgeAction::CompletePipeline,
];

#[test]
fn question_offers_exactly_the_judge_actions() {
    let questions = judge_shadow_questions();
    let json = serde_json::to_value(&questions["action"]).unwrap();
    let options: Vec<&String> = json["criteria"].as_object().unwrap().keys().collect();
    assert_eq!(options.len(), ALL.len());
    for action in ALL {
        assert!(options.iter().any(|o| *o == key(action)), "{action:?}");
        // the label used for the baseline round-trips through the real type
        assert_eq!(judge_shadow_label(action), key(action));
        assert_eq!(
            serde_json::from_value::<JudgeAction>(serde_json::json!(key(action))).unwrap(),
            action
        );
    }
}

#[test]
fn state_is_trimmed_and_marks_the_current_task() {
    let tasks: Vec<SddTask> = (0..30).map(|i| task(i, 5000)).collect();
    let ledger: Vec<String> = (0..12)
        .map(|i| format!("ledger {i} {}", "x".repeat(900)))
        .collect();
    let state = judge_shadow_state(&tasks, 2, &ledger, &"r".repeat(20_000), false, false);
    let plan = state["plan"].as_array().unwrap();
    assert_eq!(plan.len(), JUDGE_SHADOW_PLAN_ENTRIES);
    assert!(plan[2].as_str().unwrap().ends_with("<- current"));
    assert_eq!(
        state["current_task"]["body"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        JUDGE_SHADOW_TASK_BODY_CHARS
    );
    let tail = state["ledger_tail"].as_array().unwrap();
    assert_eq!(tail.len(), JUDGE_SHADOW_LEDGER_LINES);
    assert!(tail[4].as_str().unwrap().starts_with("ledger 11")); // newest kept
    assert!(
        tail.iter()
            .all(|l| l.as_str().unwrap().chars().count() <= JUDGE_SHADOW_LEDGER_LINE_CHARS)
    );
    assert_eq!(
        state["latest_role_reply"].as_str().unwrap().chars().count(),
        JUDGE_SHADOW_REPLY_CHARS
    );
    assert!(
        state.to_string().len() < 12_000,
        "stays far inside Jev's 32K window"
    );
}

#[test]
fn mode_and_missing_current_task_are_reflected() {
    let tasks = vec![task(0, 1)];
    assert_eq!(
        judge_shadow_state(&tasks, 0, &[], "", true, false)["mode"],
        "review_only"
    );
    assert_eq!(
        judge_shadow_state(&tasks, 0, &[], "", true, true)["mode"],
        "design_spec"
    );
    assert_eq!(
        judge_shadow_state(&tasks, 0, &[], "", false, false)["mode"],
        "implement"
    );
    assert!(judge_shadow_state(&tasks, 9, &[], "", false, false)["current_task"].is_null());
}

fn answer(choice: &str, confidence: Option<f64>) -> Result<Outcome, DecideError> {
    Ok(Outcome {
        response: Response {
            answers: BTreeMap::from([(
                "action".to_string(),
                Answer::Choice {
                    choice: choice.to_string(),
                    confidence,
                    probabilities: BTreeMap::new(),
                },
            )]),
            model: None,
            usage: Usage {
                cost: Some(0.00003),
                ..Usage::default()
            },
        },
        elapsed_ms: 900,
    })
}

fn compare(
    claude: &str,
    ask: impl Fn(
        &serde_json::Value,
        &BTreeMap<String, crate::decide::Question>,
    ) -> Result<Outcome, DecideError>,
) -> Vec<shadow::Row> {
    let rows = RefCell::new(vec![]);
    judge_shadow_compare(
        &serde_json::json!({"mode": "implement"}),
        claude,
        &ask,
        &|r| rows.borrow_mut().push(r.clone()),
    );
    rows.into_inner()
}

#[test]
fn logs_claude_and_jev_actions_side_by_side() {
    let rows = compare("advance_task", |_, _| answer("fix_round", Some(0.91)));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].site, "sdd_judge");
    assert_eq!(rows[0].baseline, "advance_task");
    assert_eq!(rows[0].jev.as_deref(), Some("fix_round"));
    assert_eq!(rows[0].confidence, Some(0.91));
    assert_eq!(rows[0].cost, Some(0.00003));
}

#[test]
fn unconfigured_backend_logs_nothing_and_failures_are_logged_as_fallbacks() {
    for err in [DecideError::Disabled, DecideError::NoCredentials] {
        let e = match err {
            DecideError::Disabled => DecideError::Disabled,
            _ => DecideError::NoCredentials,
        };
        assert!(
            compare("advance_task", |_, _| Err(match &e {
                DecideError::Disabled => DecideError::Disabled,
                _ => DecideError::NoCredentials,
            }))
            .is_empty()
        );
    }
    let rows = compare("advance_task", |_, _| {
        Err(DecideError::Transport("timed out".into()))
    });
    assert_eq!(rows.len(), 1);
    assert!(rows[0].error.as_deref().unwrap().contains("timed out"));
    let rows = compare("advance_task", |_, _| {
        Ok(Outcome {
            response: Response {
                answers: BTreeMap::new(),
                model: None,
                usage: Usage::default(),
            },
            elapsed_ms: 1,
        })
    });
    assert_eq!(rows[0].error.as_deref(), Some("unexpected answer shape"));
}

#[test]
fn spawn_is_a_noop_unless_opted_in() {
    // No tokio runtime here and no opt-in: must return without panicking.
    spawn_judge_shadow(
        &[task(0, 1)],
        0,
        &[],
        "reply",
        false,
        false,
        JudgeAction::AdvanceTask,
    );
}
