use flare_workflow::sqlite_store::SqliteStore;
use flare_workflow::types::*;
use flare_workflow::StateStore;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Ctx {
    value: i32,
}

impl WorkflowData for Ctx {
    fn workflow_type() -> &'static str {
        "test"
    }
}

fn seed_run(
    workflow_id: &str,
    status: WorkflowStatus,
    steps: &[(&str, StepStatus, u64, u64, u64)],
) -> WorkflowState<Ctx> {
    let run_id = WorkflowRunId::new();
    let mut state = WorkflowState::new(run_id, WorkflowId::new(workflow_id), Ctx { value: 0 });
    state.status = status;
    for (step_id, step_status, duration_ms, input_tokens, output_tokens) in steps {
        state.step_states.insert(
            StepId::new(*step_id),
            StepState {
                status: *step_status,
                duration_ms: *duration_ms,
                input_tokens: *input_tokens,
                output_tokens: *output_tokens,
                ..StepState::default()
            },
        );
    }
    state
}

#[tokio::test]
async fn workflow_metrics_aggregates_counts_and_step_breakdown() {
    let store = SqliteStore::<Ctx>::open_memory().unwrap();

    let runs = [
        seed_run(
            "wf-a",
            WorkflowStatus::Completed,
            &[("s1", StepStatus::Succeeded, 100, 10, 20)],
        ),
        seed_run(
            "wf-a",
            WorkflowStatus::Completed,
            &[("s1", StepStatus::Succeeded, 300, 30, 40)],
        ),
        seed_run(
            "wf-a",
            WorkflowStatus::Failed,
            &[("s1", StepStatus::Failed, 50, 5, 0)],
        ),
        seed_run(
            "wf-b",
            WorkflowStatus::Running,
            &[("s1", StepStatus::Running, 0, 0, 0)],
        ),
    ];
    for run in &runs {
        store.save(run.clone()).await.unwrap();
    }

    let metrics = store.workflow_metrics(MetricsFilter::default()).await.unwrap();

    assert_eq!(
        metrics.counts_by_status.get(&WorkflowStatus::Completed),
        Some(&2)
    );
    assert_eq!(
        metrics.counts_by_status.get(&WorkflowStatus::Failed),
        Some(&1)
    );
    assert_eq!(
        metrics.counts_by_status.get(&WorkflowStatus::Running),
        Some(&1)
    );
    assert_eq!(metrics.total_input_tokens, 10 + 30 + 5);
    assert_eq!(metrics.total_output_tokens, 20 + 40);

    let step = metrics
        .step_breakdown
        .get(&StepId::new("s1"))
        .expect("s1 breakdown");
    assert_eq!(step.counts_by_status.get(&StepStatus::Succeeded), Some(&2));
    assert_eq!(step.counts_by_status.get(&StepStatus::Failed), Some(&1));
    assert_eq!(step.counts_by_status.get(&StepStatus::Running), Some(&1));
    // (100 + 300 + 50 + 0) / 4
    assert_eq!(step.avg_duration_ms, Some(112.5));
}

#[tokio::test]
async fn workflow_metrics_scoped_by_workflow_id() {
    let store = SqliteStore::<Ctx>::open_memory().unwrap();

    store
        .save(seed_run(
            "wf-a",
            WorkflowStatus::Completed,
            &[("s1", StepStatus::Succeeded, 100, 1, 1)],
        ))
        .await
        .unwrap();
    store
        .save(seed_run(
            "wf-b",
            WorkflowStatus::Completed,
            &[("s1", StepStatus::Succeeded, 200, 2, 2)],
        ))
        .await
        .unwrap();

    let metrics = store
        .workflow_metrics(MetricsFilter {
            workflow_id: Some(WorkflowId::new("wf-a")),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(
        metrics.counts_by_status.get(&WorkflowStatus::Completed),
        Some(&1)
    );
    assert_eq!(metrics.total_input_tokens, 1);
    assert_eq!(metrics.total_output_tokens, 1);
    let step = metrics.step_breakdown.get(&StepId::new("s1")).unwrap();
    assert_eq!(step.avg_duration_ms, Some(100.0));
}

#[tokio::test]
async fn workflow_metrics_empty_store_returns_empty_aggregates() {
    let store = SqliteStore::<Ctx>::open_memory().unwrap();

    let metrics = store.workflow_metrics(MetricsFilter::default()).await.unwrap();

    assert!(metrics.counts_by_status.is_empty());
    assert_eq!(metrics.avg_duration_ms, None);
    assert_eq!(metrics.total_input_tokens, 0);
    assert_eq!(metrics.total_output_tokens, 0);
    assert!(metrics.step_breakdown.is_empty());
}
