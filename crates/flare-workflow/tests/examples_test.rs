//! OpenFang's four example workflows (from `.refs/openfang/docs/workflows.md`)
//! run through the flare-workflow engine via the JSON schema.

use std::sync::Arc;
use std::time::Duration;

use flare_workflow::engine::WorkflowEngine;
use flare_workflow::json::{
    JsonWorkflow, PipelineData, SendMessage, StepInvocation, compile_workflow,
};
use flare_workflow::sqlite_store::SqliteStore;
use flare_workflow::types::{WorkflowId, WorkflowStatus};

fn mock_send() -> SendMessage {
    Arc::new(|inv: StepInvocation| {
        Box::pin(async move {
            Ok((
                format!("[{} processed: {}]", inv.agent, inv.prompt),
                inv.prompt.len() as u64,
                inv.prompt.len() as u64 / 2,
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

/// Structural port of OpenFang's bundled `researcher` Hand (item #491):
/// scope -> {web,news,academic} fan_out -> collect -> fact-check ->
/// conditional follow-up -> report. Proves the ported DAG compiles and runs
/// to completion. The mock send hook echoes each step's expanded prompt
/// verbatim, and fact-check's own prompt literally contains the
/// instructional text "FOLLOWUP_NEEDED" (the marker it's told to emit), so
/// the conditional edge into `follow-up` correctly fires under the mock --
/// this proves the conditional-gate mechanism itself, not just the happy
/// path.
#[tokio::test]
async fn researcher_hand_port_compiles_and_runs() {
    let wf_json: JsonWorkflow = serde_json::from_str(
        r#"{
            "name": "researcher",
            "description": "Structural port of OpenFang's bundled 'researcher' Hand onto flare-workflow's durable DAG engine.",
            "steps": [
                {"name": "scope", "agent": "claude-code", "prompt": "Topic: {{input}}. Break this into 3-5 concrete sub-questions, and note which of three source types each depends on: general web, news, or academic literature.", "timeout_secs": 120, "error_mode": "retry", "max_retries": 1, "output_var": "brief"},
                {"name": "web-sweep", "agent": "claude-code", "prompt": "Brief:\n{{brief}}\n\nUsing mcp__flare__search (type=web), answer the web-dependent sub-questions above, with a source URL per claim.", "mode": "fan_out", "timeout_secs": 240, "error_mode": "retry", "max_retries": 1},
                {"name": "news-sweep", "agent": "claude-code", "prompt": "Brief:\n{{brief}}\n\nUsing mcp__flare__search (type=news), answer the news-dependent sub-questions above, with a source URL per claim.", "mode": "fan_out", "timeout_secs": 240, "error_mode": "retry", "max_retries": 1},
                {"name": "academic-sweep", "agent": "claude-code", "prompt": "Brief:\n{{brief}}\n\nUsing mcp__flare__search (type=academic), answer the academic-dependent sub-questions above, with a source URL per claim.", "mode": "fan_out", "timeout_secs": 240, "error_mode": "retry", "max_retries": 1},
                {"name": "gather-sources", "agent": "claude-code", "prompt": "unused (collect step is data-only)", "mode": "collect"},
                {"name": "fact-check", "agent": "claude-code", "prompt": "Combined findings:\n\n{{input}}\n\nApply a CRAAP-test pass and cross-reference claims across sweeps. If any sub-question is still unresolved, end with the exact line 'FOLLOWUP_NEEDED: <what is missing>'. Otherwise end with 'FOLLOWUP_NOT_NEEDED'.", "timeout_secs": 200, "error_mode": "retry", "max_retries": 1, "output_var": "verified"},
                {"name": "follow-up", "agent": "claude-code", "prompt": "Gap flagged:\n\n{{input}}\n\nRun one targeted follow-up search to resolve it.", "mode": {"conditional": {"condition": "FOLLOWUP_NEEDED"}}, "timeout_secs": 200, "error_mode": "skip", "output_var": "followup"},
                {"name": "report", "agent": "claude-code", "prompt": "Brief:\n{{brief}}\n\nVerified:\n{{verified}}\n\nFollow-up:\n{{followup}}\n\nWrite the final research report with inline source citations.", "timeout_secs": 200, "error_mode": "retry", "max_retries": 1}
            ]
        }"#,
    )
    .unwrap();
    let wf = compile_workflow(&wf_json, mock_send()).unwrap();
    let engine = WorkflowEngine::<PipelineData, _>::with_store(SqliteStore::open_memory().unwrap());
    engine.register_workflow(wf).unwrap();

    let run = engine
        .start_workflow(
            WorkflowId::new("researcher"),
            PipelineData,
            "durable workflow engines".into(),
        )
        .await
        .unwrap();
    engine
        .wait_for_completion(run, "researcher", Duration::from_secs(15))
        .await
        .unwrap();

    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Completed);
    // scope + fact-check both set output_var; the three fan_out sweeps
    // joined through collect into fact-check's input.
    assert!(state.variables.contains_key("brief"));
    assert!(state.variables.contains_key("verified"));
    assert!(state.variables["verified"].matches("processed").count() >= 3);
    // fact-check's own prompt contains the literal "FOLLOWUP_NEEDED" marker
    // text, so the mock's echoed output trips the conditional edge and
    // follow-up actually runs.
    assert!(state.variables.contains_key("followup"));
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
