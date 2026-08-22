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

/// Everything a step needs to invoke an agent: the target and prompt, plus
/// the per-step overrides a `JsonStep` can carry (model, extra CLI args,
/// subprocess timeouts) so a workflow author can tune all of them as data
/// instead of relying on whatever defaults the `SendMessage` hook happens to
/// hardcode.
#[derive(Debug, Clone, Default)]
pub struct StepInvocation {
    pub agent: String,
    pub prompt: String,
    pub model: Option<String>,
    pub args: Vec<String>,
    pub hard_cap_secs: Option<u64>,
    pub idle_timeout_secs: Option<u64>,
}

impl StepInvocation {
    /// An invocation with no model/args/timeout overrides — for callers
    /// (e.g. the work-item SDD loop) that don't originate from a `JsonStep`
    /// and have no per-step values to carry.
    pub fn simple(agent: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            agent: agent.into(),
            prompt: prompt.into(),
            ..Default::default()
        }
    }
}

/// Hook that sends a step invocation to an agent and returns
/// `(output, input_tokens, output_tokens)`.
pub type SendMessage = Arc<
    dyn Fn(
            StepInvocation,
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
    /// Model to request from `agent` for this step (e.g. `"sonnet"`,
    /// `"gpt-5"`) — expanded to `--model <value>` ahead of `args`.
    #[serde(default)]
    pub model: Option<String>,
    /// Extra CLI flags passed to `agent`'s headless invocation, verbatim.
    #[serde(default)]
    pub args: Vec<String>,
    /// Overrides the send hook's default hard subprocess timeout for this
    /// step.
    #[serde(default)]
    pub hard_cap_secs: Option<u64>,
    /// Overrides the send hook's default idle-output timeout for this step.
    #[serde(default)]
    pub idle_timeout_secs: Option<u64>,
    /// Skip this step (no agent dispatch) unless the condition holds.
    /// Supports `{{var}} == 'literal'` / `{{var}} != 'literal'` (string
    /// equality after `{{var}}`/`{{params.x}}` expansion) or a bare
    /// `{{var}}` truthiness check (non-empty and not `"false"`/`"0"`).
    #[serde(default)]
    pub run_if: Option<String>,
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
///
/// OpenFang positional ordering is translated to DAG edges: `sequential` and
/// friends chain onto the previous step; a `fan_out` group runs in parallel
/// (each member depends only on the step before the group) and the following
/// `collect` joins them.
pub fn compile_workflow(
    json: &JsonWorkflow,
    send: SendMessage,
) -> Result<WorkflowDefinition<PipelineData>, String> {
    let mut wf = WorkflowDefinition::new(json.name.clone(), json.name.clone());

    // Previous step that produced the input channel, per OpenFang chaining.
    let mut prev: Option<String> = None;
    // Fan-out group bookkeeping: step before the group + member indices.
    let mut fan_group: Vec<String> = Vec::new();

    for s in json.steps.iter() {
        let is_fan_out = matches!(s.mode, JsonMode::FanOut);
        let is_collect = matches!(s.mode, JsonMode::Collect);

        // Derive DAG dependencies from position.
        let deps: Vec<String> = if is_collect {
            // Collect joins every fan-out step since the group started.
            std::mem::take(&mut fan_group)
        } else if is_fan_out {
            // Every member depends only on the step before the group, so the
            // whole group runs in parallel; register this member for the
            // eventual Collect.
            fan_group.push(s.name.clone());
            prev.clone().into_iter().collect()
        } else {
            prev.clone().into_iter().collect()
        };
        let deps: Vec<&str> = deps.iter().map(String::as_str).collect();

        let executor = Arc::new(PromptExecutor {
            agent: s.agent.clone(),
            template: s.prompt.clone(),
            send: Arc::clone(&send),
            model: s.model.clone(),
            args: s.args.clone(),
            hard_cap_secs: s.hard_cap_secs,
            idle_timeout_secs: s.idle_timeout_secs,
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
        if let Some(expr) = &s.run_if {
            def = def.run_if(compile_run_if(expr));
        }
        if !deps.is_empty() {
            def = def.depends_on(&deps);
        }
        if let Some(var) = &s.output_var {
            def = def.with_output_var(var.clone());
        }
        wf = wf.add_step(def);

        // Advance the chain: sequential/collect steps produce the next input;
        // a fan-out group closes when the following step is not a fan-out.
        if !is_fan_out {
            prev = Some(s.name.clone());
            fan_group.clear();
        }
    }
    wf.validate()
        .map_err(|e| format!("invalid workflow: {e}"))?;
    Ok(wf)
}

/// Compile a `run_if` expression into a step-skip predicate. Grammar is
/// deliberately minimal: `LHS == RHS`, `LHS != RHS` (string equality after
/// `{{var}}`/`{{params.x}}` expansion on both sides, quotes trimmed off a
/// literal RHS), or a bare `LHS` truthiness check when no operator is
/// present.
fn compile_run_if(
    expr: &str,
) -> impl Fn(&WorkflowContext<PipelineData>) -> bool + Send + Sync + 'static {
    let expr = expr.to_string();
    move |ctx: &WorkflowContext<PipelineData>| {
        let expand = |s: &str| {
            expand_variables(s.trim(), &ctx.input, &ctx.variables, &ctx.params)
                .trim()
                .to_string()
        };
        if let Some((lhs, rhs)) = expr.split_once("!=") {
            expand(lhs) != expand(rhs).trim_matches(['\'', '"'])
        } else if let Some((lhs, rhs)) = expr.split_once("==") {
            expand(lhs) == expand(rhs).trim_matches(['\'', '"'])
        } else {
            let v = expand(&expr);
            !v.is_empty() && v != "false" && v != "0"
        }
    }
}

/// Executor that expands the prompt template and sends it to an agent.
struct PromptExecutor {
    agent: String,
    template: String,
    send: SendMessage,
    model: Option<String>,
    args: Vec<String>,
    hard_cap_secs: Option<u64>,
    idle_timeout_secs: Option<u64>,
}

#[async_trait]
impl StepExecutor<PipelineData> for PromptExecutor {
    async fn execute(&self, ctx: &mut WorkflowContext<PipelineData>) -> WorkflowResult<StepResult> {
        let prompt = expand_variables(&self.template, &ctx.input, &ctx.variables, &ctx.params);
        let invocation = StepInvocation {
            agent: self.agent.clone(),
            prompt,
            model: self.model.clone(),
            args: self.args.clone(),
            hard_cap_secs: self.hard_cap_secs,
            idle_timeout_secs: self.idle_timeout_secs,
        };
        let (output, input_tokens, output_tokens) =
            (self.send)(invocation)
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
        let send: SendMessage = Arc::new(move |_: StepInvocation| {
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

    #[tokio::test]
    async fn step_model_args_and_timeouts_reach_the_send_hook() {
        let captured: std::sync::Arc<std::sync::Mutex<Option<StepInvocation>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = std::sync::Arc::clone(&captured);
        let send: SendMessage = Arc::new(move |inv: StepInvocation| {
            *captured_clone.lock().unwrap() = Some(inv);
            Box::pin(async move { Ok(("ok".to_string(), 0, 0)) })
        });

        let json: JsonWorkflow = serde_json::from_str(
            r#"{
                "name": "tuned-step",
                "steps": [
                    {
                        "name": "step-a",
                        "agent": "code-reviewer",
                        "prompt": "go",
                        "model": "sonnet",
                        "args": ["--dangerously-skip-permissions"],
                        "hard_cap_secs": 42,
                        "idle_timeout_secs": 7
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
                crate::types::WorkflowId::new("tuned-step"),
                PipelineData,
                "in".into(),
            )
            .await
            .unwrap();
        engine
            .wait_for_completion(run, "wf", std::time::Duration::from_secs(10))
            .await
            .unwrap();

        let inv = captured.lock().unwrap().clone().expect("send was called");
        assert_eq!(inv.model.as_deref(), Some("sonnet"));
        assert_eq!(inv.args, vec!["--dangerously-skip-permissions"]);
        assert_eq!(inv.hard_cap_secs, Some(42));
        assert_eq!(inv.idle_timeout_secs, Some(7));
    }

    #[tokio::test]
    async fn run_if_skips_step_when_condition_false() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let calls_clone = std::sync::Arc::clone(&calls);
        // Echo the prompt verbatim so `set-flag`'s output_var takes on exactly
        // the string its prompt names, letting the run_if condition below
        // check against a known value.
        let send: SendMessage = Arc::new(move |inv: StepInvocation| {
            calls_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move { Ok((inv.prompt, 0, 0)) })
        });

        let json: JsonWorkflow = serde_json::from_str(
            r#"{
                "name": "conditional-pipeline",
                "steps": [
                    {
                        "name": "set-flag",
                        "agent": "setter",
                        "prompt": "cached",
                        "output_var": "cache_check"
                    },
                    {
                        "name": "maybe-analyze",
                        "agent": "analyzer",
                        "prompt": "analyze",
                        "run_if": "{{cache_check}} != 'cached'"
                    },
                    {
                        "name": "always-record",
                        "agent": "recorder",
                        "prompt": "record"
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
                crate::types::WorkflowId::new("conditional-pipeline"),
                PipelineData,
                "go".into(),
            )
            .await
            .unwrap();
        engine
            .wait_for_completion(run, "wf", std::time::Duration::from_secs(10))
            .await
            .unwrap();

        let state = engine.get_status(run).await.unwrap();
        assert_eq!(state.status, WorkflowStatus::Completed);
        // set-flag and always-record ran; maybe-analyze was skipped (no dispatch).
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn run_if_runs_step_when_condition_true() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let calls_clone = std::sync::Arc::clone(&calls);
        let send: SendMessage = Arc::new(move |inv: StepInvocation| {
            calls_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move { Ok((inv.prompt, 0, 0)) })
        });

        let json: JsonWorkflow = serde_json::from_str(
            r#"{
                "name": "conditional-pipeline-2",
                "steps": [
                    {
                        "name": "set-flag",
                        "agent": "setter",
                        "prompt": "stale",
                        "output_var": "cache_check"
                    },
                    {
                        "name": "maybe-analyze",
                        "agent": "analyzer",
                        "prompt": "analyze",
                        "run_if": "{{cache_check}} != 'cached'"
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
                crate::types::WorkflowId::new("conditional-pipeline-2"),
                PipelineData,
                "go".into(),
            )
            .await
            .unwrap();
        engine
            .wait_for_completion(run, "wf", std::time::Duration::from_secs(10))
            .await
            .unwrap();

        let state = engine.get_status(run).await.unwrap();
        assert_eq!(state.status, WorkflowStatus::Completed);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}
