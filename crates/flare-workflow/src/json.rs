//! OpenFang-style JSON workflow definitions, compiled onto the engine as
//! agent-prompt steps (schema + semantics ported from OpenFang, MIT/Apache-2.0).
//!
//! A JSON workflow routes each step's expanded prompt to a named agent via a
//! caller-supplied `SendMessage` hook, so workflows can be authored as data and
//! run through the durable engine without writing Rust step closures.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::definition::{StepDefinition, WorkflowDefinition};
use crate::executor::StepExecutor;
use crate::types::*;
use crate::variables::expand_variables;

/// The typed workflow data for JSON-defined agent pipelines: the string
/// pipeline (`{{input}}` / `{{var}}`) is the whole payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PipelineData;

impl WorkflowData for PipelineData {
    fn workflow_type() -> &'static str {
        "pipeline"
    }
}

/// Hook that sends an expanded prompt to an agent and returns
/// `(output, input_tokens, output_tokens)`.
pub type SendMessage = Arc<
    dyn Fn(
            String,
            String,
        ) -> Pin<Box<dyn Future<Output = Result<(String, u64, u64), String>> + Send>>
        + Send
        + Sync,
>;

/// JSON workflow definition (OpenFang schema).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonWorkflow {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub steps: Vec<JsonStep>,
}

/// A single JSON step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonStep {
    pub name: String,
    pub agent: String,
    #[serde(default = "default_prompt")]
    pub prompt: String,
    #[serde(default)]
    pub mode: JsonMode,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub error_mode: JsonErrorMode,
    #[serde(default)]
    pub max_retries: u32,
    #[serde(default)]
    pub output_var: Option<String>,
}

fn default_prompt() -> String {
    "{{input}}".to_string()
}

fn default_timeout() -> u64 {
    120
}

/// Step execution mode (OpenFang `StepMode`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonMode {
    #[default]
    Sequential,
    FanOut,
    Collect,
    Conditional {
        condition: String,
    },
    Loop {
        max_iterations: u32,
        until: String,
    },
    Sleep {
        duration_secs: u64,
    },
    WaitEvent {
        name: String,
        timeout_secs: u64,
    },
}

/// Per-step error handling (OpenFang `ErrorMode`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonErrorMode {
    #[default]
    Fail,
    Skip,
    Retry,
}

/// Compile a JSON workflow into an engine-ready definition. Each step's
/// executor expands `{{input}}`/`{{var}}` and dispatches via `send`.
pub fn compile_workflow(
    json: &JsonWorkflow,
    send: SendMessage,
) -> Result<WorkflowDefinition<PipelineData>, String> {
    let mut wf = WorkflowDefinition::new(json.name.clone(), json.name.clone());
    for s in &json.steps {
        let executor = Arc::new(PromptExecutor {
            agent: s.agent.clone(),
            template: s.prompt.clone(),
            send: Arc::clone(&send),
        });
        let mut def = StepDefinition::new(s.name.clone(), s.name.clone(), executor)
            .with_timeout(std::time::Duration::from_secs(s.timeout_secs))
            .with_mode(match &s.mode {
                JsonMode::Sequential => StepMode::Sequential,
                JsonMode::FanOut => StepMode::FanOut,
                JsonMode::Collect => StepMode::Collect,
                JsonMode::Conditional { condition } => StepMode::Conditional {
                    condition: condition.clone(),
                },
                JsonMode::Loop {
                    max_iterations,
                    until,
                } => StepMode::Loop {
                    max_iterations: *max_iterations,
                    until: until.clone(),
                },
                JsonMode::Sleep { duration_secs } => StepMode::Sleep {
                    duration_secs: *duration_secs,
                },
                JsonMode::WaitEvent { name, timeout_secs } => StepMode::WaitEvent {
                    name: name.clone(),
                    timeout_secs: *timeout_secs,
                },
            })
            .with_error_mode(match s.error_mode {
                JsonErrorMode::Fail => ErrorMode::Fail,
                JsonErrorMode::Skip => ErrorMode::Skip,
                JsonErrorMode::Retry => ErrorMode::Retry {
                    max_retries: s.max_retries,
                },
            });
        if let Some(var) = &s.output_var {
            def = def.with_output_var(var.clone());
        }
        wf = wf.add_step(def);
    }
    wf.validate()
        .map_err(|e| format!("invalid workflow: {e}"))?;
    Ok(wf)
}

/// Executor that expands the prompt template and sends it to an agent.
struct PromptExecutor {
    agent: String,
    template: String,
    send: SendMessage,
}

#[async_trait]
impl StepExecutor<PipelineData> for PromptExecutor {
    async fn execute(&self, ctx: &mut WorkflowContext<PipelineData>) -> WorkflowResult<StepResult> {
        let prompt = expand_variables(&self.template, &ctx.input, &ctx.variables);
        let (output, input_tokens, output_tokens) = (self.send)(self.agent.clone(), prompt)
            .await
            .map_err(|e| WorkflowError::StepFailed {
            step_id: StepId::new(&self.agent),
            message: e,
        })?;
        ctx.output = output;
        ctx.input_tokens = input_tokens;
        ctx.output_tokens = output_tokens;
        Ok(StepResult::Success)
    }

    fn is_retryable(&self, error: &WorkflowError) -> bool {
        // Agent/prompt steps retry on any step failure by default.
        !matches!(error, WorkflowError::ShuttingDown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::WorkflowEngine;
    use crate::store::InMemoryStore;

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

    #[tokio::test]
    async fn compiles_and_runs_openfang_style_workflow() {
        let json: JsonWorkflow = serde_json::from_str(
            r#"{
                "name": "code-review-pipeline",
                "description": "analyze -> security-check -> summary",
                "steps": [
                    {
                        "name": "analyze",
                        "agent": "code-reviewer",
                        "prompt": "Analyze: {{input}}",
                        "mode": "sequential",
                        "timeout_secs": 30,
                        "error_mode": "fail",
                        "output_var": "analysis"
                    },
                    {
                        "name": "summary",
                        "agent": "writer",
                        "prompt": "Summarize analysis: {{analysis}}",
                        "mode": "sequential",
                        "timeout_secs": 30,
                        "error_mode": "fail"
                    }
                ]
            }"#,
        )
        .unwrap();

        let wf = compile_workflow(&json, mock_send()).unwrap();
        let engine = WorkflowEngine::<PipelineData, InMemoryStore<PipelineData>>::new();
        engine.register_workflow(wf).unwrap();

        let run = engine
            .start_workflow(
                crate::types::WorkflowId::new("code-review-pipeline"),
                PipelineData,
                "the code".into(),
            )
            .await
            .unwrap();
        engine
            .wait_for_completion(run, "wf", std::time::Duration::from_secs(10))
            .await
            .unwrap();

        let state = engine.get_status(run).await.unwrap();
        assert_eq!(state.status, WorkflowStatus::Completed);
        // Variable captured from step 1 is referenced by step 2.
        assert!(state.variables.contains_key("analysis"));
        assert!(state.input.contains("processed"));
    }

    #[tokio::test]
    async fn loop_until_approval_workflow() {
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let cc = std::sync::Arc::clone(&counter);
        let send: SendMessage = Arc::new(move |_: String, _: String| {
            let cc = std::sync::Arc::clone(&cc);
            Box::pin(async move {
                let n = cc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok((
                    if n >= 2 {
                        "APPROVED draft".to_string()
                    } else {
                        "needs work".to_string()
                    },
                    1,
                    1,
                ))
            })
        });

        let json: JsonWorkflow = serde_json::from_str(
            r#"{
                "name": "iterative-refinement",
                "steps": [
                    {
                        "name": "review-and-refine",
                        "agent": "reviewer",
                        "prompt": "Review: {{input}}",
                        "mode": { "loop": { "max_iterations": 5, "until": "APPROVED" } },
                        "timeout_secs": 30,
                        "error_mode": "fail"
                    }
                ]
            }"#,
        )
        .unwrap();

        let wf = compile_workflow(&json, send).unwrap();
        let engine = WorkflowEngine::<PipelineData, InMemoryStore<PipelineData>>::new();
        engine.register_workflow(wf).unwrap();
        let run = engine
            .start_workflow(
                crate::types::WorkflowId::new("iterative-refinement"),
                PipelineData,
                "draft".into(),
            )
            .await
            .unwrap();
        engine
            .wait_for_completion(run, "wf", std::time::Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 3);
    }
}
