//! OpenFang's four example workflows (from `.refs/openfang/docs/workflows.md`)
//! run through the flare-workflow engine via the JSON schema.

use std::sync::Arc;
use std::time::Duration;

use flare_workflow::engine::WorkflowEngine;
use flare_workflow::json::{JsonWorkflow, PipelineData, SendMessage, compile_workflow};
use flare_workflow::sqlite_store::SqliteStore;
use flare_workflow::types::{WorkflowId, WorkflowStatus};

fn mock_send() -> SendMessage {
    Arc::new(|agent: String, prompt: String| {
        Box::pin(async move {
            Ok((
                format!("[{agent} processed: {prompt}]"),
                prompt.len() as u64,
                prompt.len() as u64 / 2,
            ))
        })
    })
}

async fn run_json(json: &str) -> String {
    let wf_json: JsonWorkflow = serde_json::from_str(json).unwrap();
    let wf = compile_workflow(&wf_json, mock_send()).unwrap();
    let name = wf.name.clone();
    let engine = WorkflowEngine::<PipelineData, _>::with_store(SqliteStore::open_memory().unwrap());
    engine.register_workflow(wf).unwrap();
    let run = engine
        .start_workflow(
            WorkflowId::new(name.clone()),
            PipelineData,
            "seed input".into(),
        )
        .await
        .unwrap();
    engine
        .wait_for_completion(run, &name, Duration::from_secs(15))
        .await
        .unwrap();
    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Completed);
    state.input
}

#[tokio::test]
async fn code_review_pipeline() {
    let out = run_json(
        r#"{
            "name": "code-review-pipeline",
            "description": "analyze code, review for issues, produce summary",
            "steps": [
                {"name": "analyze", "agent": "code-reviewer", "prompt": "Analyze the following code for bugs:\n\n{{input}}", "timeout_secs": 60, "output_var": "analysis"},
                {"name": "security-check", "agent": "security-auditor", "prompt": "Review this analysis for security issues:\n\n{{analysis}}", "timeout_secs": 60, "output_var": "security_review"},
                {"name": "summary", "agent": "writer", "prompt": "Write a code review summary.\n\nAnalysis:\n{{analysis}}\n\nSecurity:\n{{security_review}}", "timeout_secs": 60}
            ]
        }"#,
    )
    .await;
    assert!(out.contains("processed"));
}

#[tokio::test]
async fn research_and_write_with_conditional() {
    let out = run_json(
        r#"{
            "name": "research-and-write",
            "description": "research, outline, write, conditional fact-check",
            "steps": [
                {"name": "research", "agent": "researcher", "prompt": "Research: {{input}}", "timeout_secs": 60, "output_var": "research"},
                {"name": "outline", "agent": "planner", "prompt": "Outline from research:\n\n{{research}}", "timeout_secs": 60, "output_var": "outline"},
                {"name": "write", "agent": "writer", "prompt": "Write article.\n\nOutline:\n{{outline}}", "timeout_secs": 60, "output_var": "article"},
                {"name": "fact-check", "agent": "analyst", "prompt": "Fact-check:\n\n{{article}}", "mode": {"conditional": {"condition": "claim"}}, "timeout_secs": 60, "error_mode": "skip"}
            ]
        }"#,
    )
    .await;
    assert!(out.contains("processed"));
}

#[tokio::test]
async fn brainstorm_fanout_collect() {
    let out = run_json(
        r#"{
            "name": "brainstorm",
            "description": "parallel brainstorm then synthesize",
            "steps": [
                {"name": "creative-ideas", "agent": "writer", "prompt": "Brainstorm 5 creative ideas for: {{input}}", "mode": "fan_out", "timeout_secs": 60},
                {"name": "technical-ideas", "agent": "architect", "prompt": "Brainstorm 5 technically feasible ideas for: {{input}}", "mode": "fan_out", "timeout_secs": 60},
                {"name": "business-ideas", "agent": "analyst", "prompt": "Brainstorm 5 business ideas for: {{input}}", "mode": "fan_out", "timeout_secs": 60},
                {"name": "gather", "agent": "planner", "prompt": "unused", "mode": "collect"},
                {"name": "synthesize", "agent": "orchestrator", "prompt": "Synthesize the brainstorm results:\n\n{{input}}", "timeout_secs": 60}
            ]
        }"#,
    )
    .await;
    // The collect joined the three fan-out outputs, so synthesizer's input
    // contains multiple processed outputs.
    assert!(out.matches("processed").count() >= 3);
}

/// `params` flows from `start_workflow_with_params` through `{{params.x}}`
/// dotted-path expansion into a step's prompt (item #126).
#[tokio::test]
async fn params_flow_through_to_prompt_expansion() {
    let wf_json: JsonWorkflow = serde_json::from_str(
        r#"{
            "name": "params-roundtrip",
            "steps": [
                {"name": "greet", "agent": "writer", "prompt": "Hello {{params.userId}}, task: {{params.metadata.task}}"}
            ]
        }"#,
    )
    .unwrap();
    let wf = compile_workflow(&wf_json, mock_send()).unwrap();
    let engine = WorkflowEngine::<PipelineData, _>::with_store(SqliteStore::open_memory().unwrap());
    engine.register_workflow(wf).unwrap();

    let params = serde_json::json!({"userId": "u-42", "metadata": {"task": "review"}});
    let run = engine
        .start_workflow_with_params(
            WorkflowId::new("params-roundtrip"),
            PipelineData,
            "seed".into(),
            params,
        )
        .await
        .unwrap();
    engine
        .wait_for_completion(run, "params-roundtrip", Duration::from_secs(15))
        .await
        .unwrap();

    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Completed);
    assert!(state.input.contains("Hello u-42, task: review"));
}

#[tokio::test]
async fn iterative_refinement_loop() {
    let out = run_json(
        r#"{
            "name": "iterative-refinement",
            "description": "refine until approved or max iterations",
            "steps": [
                {"name": "first-draft", "agent": "writer", "prompt": "Write a first draft about: {{input}}", "timeout_secs": 60, "output_var": "draft"},
                {"name": "review-and-refine", "agent": "code-reviewer", "prompt": "Review this draft:\n\n{{input}}", "mode": {"loop": {"max_iterations": 4, "until": "APPROVED"}}, "timeout_secs": 60}
            ]
        }"#,
    )
    .await;
    assert!(out.contains("processed"));
}
