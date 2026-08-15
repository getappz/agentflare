use std::sync::Arc;
use std::time::Duration;

use flare_workflow::engine::WorkflowEngine;
use flare_workflow::executor::FunctionStep;
use flare_workflow::sqlite_store::SqliteStore;
use flare_workflow::types::*;
use flare_workflow::{StepDefinition, WorkflowDefinition};

/// A `WorkflowData` shaped like a real agent-pipeline payload (a struct with
/// fields) -- e.g. the internal SDD work-item pipeline's own context type.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct StructPayload {
    note: String,
}

impl WorkflowData for StructPayload {
    fn workflow_type() -> &'static str {
        "struct-payload"
    }
}

/// A `WorkflowData` shaped like `flare_workflow::json::PipelineData` -- a
/// unit struct, exactly what fails to deserialize a `StructPayload` row.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct UnitPayload;

impl WorkflowData for UnitPayload {
    fn workflow_type() -> &'static str {
        "unit-payload"
    }
}

fn trivial_step<D: WorkflowData>() -> StepDefinition<D> {
    StepDefinition::new(
        "only",
        "only",
        Arc::new(FunctionStep::new(|_ctx: &mut WorkflowContext<D>| {
            Box::pin(async move { Ok(StepResult::Success) })
        })),
    )
}

/// Regression test for the `workflow_runs` cross-engine type collision: two
/// `WorkflowEngine<D, SqliteStore<D>>` instances sharing one SQLite file with
/// *different* `D` types must not break each other. Before the fix,
/// `evict_old_runs_if_needed` -> `list_all()` deserialized every row in the
/// shared table as the caller's own `D` and hard-failed `start_workflow` the
/// moment any foreign-typed row existed in the table -- see item #474.
#[tokio::test]
async fn a_foreign_typed_row_does_not_break_other_engines_sharing_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.db");

    // Engine A writes a row shaped like `StructPayload`.
    {
        let engine =
            WorkflowEngine::<StructPayload, _>::with_store(SqliteStore::open_file(&path).unwrap());
        engine
            .register_workflow(
                WorkflowDefinition::new("wf-a", "wf-a").add_step(trivial_step::<StructPayload>()),
            )
            .unwrap();
        let run = engine
            .start_workflow(
                WorkflowId::new("wf-a"),
                StructPayload { note: "hi".into() },
                "seed".into(),
            )
            .await
            .unwrap();
        engine
            .wait_for_completion(run, "wf-a", Duration::from_secs(10))
            .await
            .unwrap();
    }

    // Engine B, a *different* D, pointed at the same file: must still be
    // able to start and complete its own run despite Engine A's row.
    let engine =
        WorkflowEngine::<UnitPayload, _>::with_store(SqliteStore::open_file(&path).unwrap());
    engine
        .register_workflow(
            WorkflowDefinition::new("wf-b", "wf-b").add_step(trivial_step::<UnitPayload>()),
        )
        .unwrap();
    let run = engine
        .start_workflow(WorkflowId::new("wf-b"), UnitPayload, "seed".into())
        .await
        .expect("start_workflow must not fail because of an unrelated engine's row");
    engine
        .wait_for_completion(run, "wf-b", Duration::from_secs(10))
        .await
        .unwrap();

    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Completed);
}
