use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeZone, Utc};

use flare_workflow::definition::{StepDefinition, WorkflowDefinition};
use flare_workflow::executor::FunctionStep;
use flare_workflow::types::{
    BackoffStrategy, ErrorMode, FailureAction, JournalEntry, RetryPolicy, StepId, StepMode,
    WorkflowData, WorkflowId, WorkflowRunId, WorkflowStatus,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct TestData {
    value: i32,
}

impl WorkflowData for TestData {
    fn workflow_type() -> &'static str {
        "test"
    }
}

fn ok_step(id: &str) -> StepDefinition<TestData> {
    StepDefinition::new(id, id, Arc::new(FunctionStep::new(|ctx: &mut flare_workflow::WorkflowContext<TestData>| {
        ctx.data.value += 1;
        Box::pin(async move { Ok(flare_workflow::StepResult::Success) })
    })))
}

#[test]
fn ids_display_and_roundtrip() {
    let wid = WorkflowId::new("wf-1");
    assert_eq!(wid.to_string(), "wf-1");

    let rid = WorkflowRunId::new();
    let json = serde_json::to_string(&rid).unwrap();
    let back: WorkflowRunId = serde_json::from_str(&json).unwrap();
    assert_eq!(rid, back);

    let sid = StepId::new("step-a");
    assert_eq!(sid.to_string(), "step-a");
}

#[test]
fn step_mode_serde_roundtrip() {
    let modes = vec![
        StepMode::Sequential,
        StepMode::FanOut,
        StepMode::Collect,
        StepMode::Conditional {
            condition: "ERROR".into(),
        },
        StepMode::Loop {
            max_iterations: 5,
            until: "DONE".into(),
        },
        StepMode::Sleep { duration_secs: 30 },
        StepMode::WaitEvent {
            name: "approve".into(),
            timeout_secs: 600,
        },
    ];
    for mode in modes {
        let json = serde_json::to_string(&mode).unwrap();
        let back: StepMode = serde_json::from_str(&json).unwrap();
        assert_eq!(mode, back);
    }
}

#[test]
fn journal_entry_completable_invariant() {
    let pending = JournalEntry::StepRun {
        step_id: StepId::new("s"),
        attempt: 1,
        result: None,
    };
    assert!(!pending.is_completed());

    let completed = JournalEntry::StepRun {
        step_id: StepId::new("s"),
        attempt: 1,
        result: Some(flare_workflow::EntryResult::Success(b"ok".to_vec())),
    };
    assert!(completed.is_completed());

    let wait = JournalEntry::WaitEvent {
        name: "approve".into(),
        result: None,
    };
    assert!(!wait.is_completed());
    assert_eq!(wait.entry_type(), "wait_event");

    let sleep = JournalEntry::Sleep {
        step_id: StepId::new("s"),
        wake_at: Utc::now(),
        result: None,
    };
    assert!(!sleep.is_completed());
    assert_eq!(sleep.entry_type(), "sleep");
}

#[test]
fn validate_accepts_valid_dag() {
    let mut wf = WorkflowDefinition::new("wf", "wf")
        .add_step(ok_step("a"))
        .add_step(ok_step("b").depends_on(&["a"]))
        .add_step(ok_step("c").depends_on(&["a", "b"]));
    assert!(wf.validate().is_ok());

    let mut parallel = WorkflowDefinition::new("wf2", "wf2")
        .add_step(ok_step("a"))
        .add_step(ok_step("b"))
        .add_step(ok_step("join").depends_on(&["a", "b"]));
    assert!(parallel.validate().is_ok());
}

#[test]
fn validate_rejects_missing_dependency() {
    let mut wf = WorkflowDefinition::new("wf", "wf").add_step(ok_step("a").depends_on(&["ghost"]));
    let err = wf.validate().unwrap_err();
    assert!(matches!(err, flare_workflow::ValidationError::MissingDependency { .. }));
}

#[test]
fn validate_rejects_cycles() {
    let mut wf = WorkflowDefinition::new("wf", "wf")
        .add_step(ok_step("a").depends_on(&["b"]))
        .add_step(ok_step("b").depends_on(&["a"]));
    let err = wf.validate().unwrap_err();
    assert!(matches!(err, flare_workflow::ValidationError::CycleDetected(_)));
}

#[test]
fn depends_on_any_initial_steps_excluded() {
    // A step with only depends_on_any still needs one to complete, so it must
    // not be an initial step.
    let mut wf = WorkflowDefinition::new("wf", "wf")
        .add_step(ok_step("a"))
        .add_step(ok_step("b").depends_on_any(&["a"]));
    wf.validate().unwrap();
    assert_eq!(wf.get_initial_step_indices(), &[0]);
    assert_eq!(wf.get_dependent_indices(&StepId::new("a")), &[1]);
}

#[test]
fn retry_policy_defaults_are_exponential() {
    let p = RetryPolicy::default();
    assert_eq!(p.max_attempts, 3);
    assert!(matches!(
        p.backoff,
        BackoffStrategy::Exponential { .. }
    ));
}

#[test]
fn builder_configured_step_carries_semantics() {
    let step = ok_step("s")
        .with_mode(StepMode::Loop {
            max_iterations: 4,
            until: "APPROVED".into(),
        })
        .with_error_mode(ErrorMode::Retry { max_retries: 2 })
        .with_failure_action(FailureAction::ContinueNextStep)
        .with_timeout(Duration::from_secs(30));
    assert!(matches!(step.mode, StepMode::Loop { max_iterations: 4, .. }));
    assert!(matches!(step.error_mode, ErrorMode::Retry { max_retries: 2 }));
    assert_eq!(step.on_failure, FailureAction::ContinueNextStep);
    assert_eq!(step.timeout, Some(Duration::from_secs(30)));
}

#[test]
fn scheduled_at_utc_roundtrip() {
    let t = Utc.with_ymd_and_hms(2026, 8, 12, 12, 0, 0).unwrap();
    let step = ok_step("s").scheduled_at(t);
    assert_eq!(step.scheduled_at, Some(t));
}

#[test]
fn workflow_status_serde_roundtrip() {
    for s in [
        WorkflowStatus::Pending,
        WorkflowStatus::Running,
        WorkflowStatus::Completed,
        WorkflowStatus::Failed,
        WorkflowStatus::Cancelled,
    ] {
        let json = serde_json::to_string(&s).unwrap();
        let back: WorkflowStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }
}
