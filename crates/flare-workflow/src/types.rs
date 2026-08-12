//! Core workflow types: identifiers, step modes, retry policies, journal
//! entries, and instance state.
//!
//! Design lineage:
//! - Typed context + retry/backoff policy model: SMG `wfaas` (Apache-2.0).
//! - Step semantics (Sequential/FanOut/Collect/Conditional/Loop) and error
//!   modes (Fail/Skip/Retry): OpenFang (MIT/Apache-2.0).
//! - Journal/durable-execution model (CompletableEntry invariant, step
//!   memoization, durable Sleep/WaitEvent): Restate — design only, no code.

use std::{collections::HashMap, fmt, time::Duration};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;

/// Workflow data passed through steps as the typed shared context.
///
/// Must be serializable so workflow state can be persisted and recovered.
pub trait WorkflowData: Serialize + DeserializeOwned + Send + Sync + Clone + 'static {
    /// Human-readable name for logging and identification.
    fn workflow_type() -> &'static str;
}

/// Unique identifier for a workflow definition.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkflowId(String);

impl WorkflowId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl fmt::Display for WorkflowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Unique identifier for a running workflow instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkflowRunId(Uuid);

impl WorkflowRunId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for WorkflowRunId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for WorkflowRunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for WorkflowRunId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}

/// Unique identifier for a workflow step.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StepId(String);

impl StepId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl fmt::Display for StepId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Orchestration semantics for a step, layered on top of the DAG schedule.
///
/// `Sequential` is the default: the step runs once its DAG dependencies are
/// satisfied. `FanOut`/`Collect` express the OpenFang parallel-group pattern.
/// `Conditional`/`Loop` gate on the previous step's output. `Sleep` and
/// `WaitEvent` are durable waits (persisted in the journal).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepMode {
    /// Execute once, after all DAG dependencies complete.
    #[default]
    Sequential,
    /// Run in parallel with sibling `FanOut` steps; outputs collected by a
    /// following `Collect` step.
    FanOut,
    /// Data-only step: joins the preceding fan-out group's outputs into one
    /// input channel.
    Collect,
    /// Skip this step unless the previous step's output contains `condition`
    /// (case-insensitive).
    Conditional { condition: String },
    /// Repeat until the output contains `until` or `max_iterations` elapse.
    Loop { max_iterations: u32, until: String },
    /// Durable delay: suspends the run for `duration_secs` (survives restart).
    Sleep { duration_secs: u64 },
    /// Durable promise: waits for `complete_event` within `timeout_secs`.
    WaitEvent { name: String, timeout_secs: u64 },
}

/// How a step failure is handled.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorMode {
    /// Abort the workflow on error.
    #[default]
    Fail,
    /// Skip this step on error and continue the workflow.
    Skip,
    /// Retry the step up to `max_retries` times before failing.
    Retry { max_retries: u32 },
}

/// What the engine does when a step exhausts its retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureAction {
    /// Stop the entire workflow.
    FailWorkflow,
    /// Skip this step and continue to the next.
    ContinueNextStep,
    /// Keep retrying indefinitely until manual intervention.
    RetryIndefinitely,
}

/// Retry policy for a step (or the workflow default).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff: BackoffStrategy,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            backoff: BackoffStrategy::Exponential {
                base: Duration::from_secs(1),
                max: Duration::from_secs(30),
            },
        }
    }
}

/// Backoff strategy between retry attempts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BackoffStrategy {
    /// Fixed delay between retries.
    Fixed(Duration),
    /// Exponential backoff with base and max interval.
    Exponential { base: Duration, max: Duration },
    /// Linear backoff, increasing by `increment` up to `max`.
    Linear { increment: Duration, max: Duration },
}

/// Workflow execution status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatus {
    Pending,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

/// Step execution status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Retrying,
    Skipped,
}

/// Per-step execution state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepState {
    pub status: StepStatus,
    pub attempt: u32,
    pub last_error: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Token accounting + duration recorded on the last (successful) attempt.
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub duration_ms: u64,
}

impl Default for StepState {
    fn default() -> Self {
        Self {
            status: StepStatus::Pending,
            attempt: 0,
            last_error: None,
            started_at: None,
            completed_at: None,
            input_tokens: 0,
            output_tokens: 0,
            duration_ms: 0,
        }
    }
}

/// Result of a durable entry once completed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryResult {
    Success(Vec<u8>),
    Failure {
        code: u32,
        message: String,
        metadata: Vec<(String, String)>,
    },
}

impl EntryResult {
    pub fn success<T: Serialize>(value: &T) -> Self {
        Self::Success(serde_json::to_vec(value).unwrap_or_default())
    }

    pub fn from_json(value: &serde_json::Value) -> Self {
        Self::Success(serde_json::to_vec(value).unwrap_or_default())
    }
}

/// One entry in a workflow run's durable journal (append-only, per-run
/// monotonic sequence).
///
/// CompletableEntry invariant (from Restate's design): an entry is **pending**
/// while its result field is `None` and **completed** once it is `Some`.
/// Recovery replays the journal and only re-executes pending entries, which
/// gives exactly-once step execution across crashes and retries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JournalEntry {
    /// The run's initial input.
    Input { value: Vec<u8> },
    /// A memoized step execution. `result` is written when the step completes;
    /// completed `StepRun` entries are never re-executed on replay.
    StepRun {
        step_id: StepId,
        attempt: u32,
        result: Option<EntryResult>,
    },
    /// Read per-run key/value state.
    StateGet {
        key: String,
        value: Option<EntryResult>,
    },
    /// Write per-run key/value state.
    StateSet { key: String, value: Vec<u8> },
    /// Clear per-run key/value state.
    StateClear { key: String },
    /// Durable timer: fires once wall-clock passes `wake_at`.
    Sleep {
        step_id: StepId,
        wake_at: DateTime<Utc>,
        result: Option<EntryResult>,
    },
    /// Durable promise: resolved by `complete_event` or by timeout.
    WaitEvent {
        name: String,
        result: Option<EntryResult>,
    },
    /// The run's final output.
    Output { result: EntryResult },
}

impl JournalEntry {
    /// True once this entry carries a completed result (CompletableEntry).
    pub fn is_completed(&self) -> bool {
        match self {
            JournalEntry::Input { .. } => true,
            JournalEntry::StepRun { result, .. } => result.is_some(),
            JournalEntry::StateGet { value, .. } => value.is_some(),
            JournalEntry::StateSet { .. } | JournalEntry::StateClear { .. } => true,
            JournalEntry::Sleep { result, .. } => result.is_some(),
            JournalEntry::WaitEvent { result, .. } => result.is_some(),
            JournalEntry::Output { .. } => true,
        }
    }

    /// A short discriminator used as the journal table's `entry_type`.
    pub fn entry_type(&self) -> &'static str {
        match self {
            JournalEntry::Input { .. } => "input",
            JournalEntry::StepRun { .. } => "step_run",
            JournalEntry::StateGet { .. } => "state_get",
            JournalEntry::StateSet { .. } => "state_set",
            JournalEntry::StateClear { .. } => "state_clear",
            JournalEntry::Sleep { .. } => "sleep",
            JournalEntry::WaitEvent { .. } => "wait_event",
            JournalEntry::Output { .. } => "output",
        }
    }
}

/// Typed context shared between steps. Fully serializable (fields marked
/// `#[serde(skip)]` are not persisted).
///
/// Alongside the typed `data`, the context carries the OpenFang-style string
/// pipeline: `input` is the current `{{input}}` channel, `output` is what the
/// step produced (the executor sets it; the engine chains it forward).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(serialize = "D: Serialize", deserialize = "D: DeserializeOwned"))]
pub struct WorkflowContext<D: WorkflowData> {
    pub run_id: WorkflowRunId,
    pub data: D,
    /// Current `{{input}}` pipeline channel for this step.
    #[serde(default)]
    pub input: String,
    /// What this step produced; the engine reads it back after execution.
    #[serde(default)]
    pub output: String,
    /// Named variables for `{{var}}` prompt expansion (mirrors run state).
    #[serde(default)]
    pub variables: HashMap<String, String>,
    /// Token accounting reported by the executor (for agent-prompt steps).
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

impl<D: WorkflowData> WorkflowContext<D> {
    pub fn new(run_id: WorkflowRunId, data: D) -> Self {
        Self {
            run_id,
            data,
            input: String::new(),
            output: String::new(),
            variables: HashMap::new(),
            input_tokens: 0,
            output_tokens: 0,
        }
    }
}

/// Full state of a running workflow instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(serialize = "D: Serialize", deserialize = "D: DeserializeOwned"))]
pub struct WorkflowState<D: WorkflowData> {
    pub run_id: WorkflowRunId,
    pub workflow_id: WorkflowId,
    pub status: WorkflowStatus,
    pub current_step: Option<StepId>,
    pub step_states: HashMap<StepId, StepState>,
    pub context: WorkflowContext<D>,
    /// String pipeline channel (`{{input}}` for the next step).
    pub input: String,
    pub output: Option<String>,
    pub error: Option<String>,
    /// Named variables captured via `output_var` (OpenFang-style templating).
    pub variables: HashMap<String, String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl<D: WorkflowData> WorkflowState<D> {
    pub fn new(run_id: WorkflowRunId, workflow_id: WorkflowId, data: D) -> Self {
        let now = Utc::now();
        Self {
            run_id,
            workflow_id,
            status: WorkflowStatus::Pending,
            current_step: None,
            step_states: HashMap::new(),
            context: WorkflowContext::new(run_id, data),
            input: String::new(),
            output: None,
            error: None,
            variables: HashMap::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

/// Result returned by a step execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepResult {
    Success,
    Failure,
    Skip,
}

/// Errors surfaced by the workflow engine.
#[derive(Debug, Clone, thiserror::Error)]
pub enum WorkflowError {
    #[error("workflow run not found: {0}")]
    NotFound(WorkflowRunId),

    #[error("workflow definition not found: {0}")]
    DefinitionNotFound(WorkflowId),

    #[error("step failed: {step_id} - {message}")]
    StepFailed { step_id: StepId, message: String },

    #[error("step timed out: {step_id}")]
    StepTimeout { step_id: StepId },

    #[error("workflow cancelled: {0}")]
    Cancelled(WorkflowRunId),

    #[error("invalid state transition: {from:?} -> {to:?}")]
    InvalidStateTransition {
        from: WorkflowStatus,
        to: WorkflowStatus,
    },

    #[error("engine is shutting down, not accepting new workflows")]
    ShuttingDown,

    #[error("journal error: {0}")]
    Journal(String),

    #[error("storage error: {0}")]
    Store(String),
}

pub type WorkflowResult<T> = Result<T, WorkflowError>;
