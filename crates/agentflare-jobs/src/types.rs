use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Exited,
    Killed,
    Failed,
}

impl JobState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, JobState::Exited | JobState::Killed | JobState::Failed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentJob {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
    pub timeout_secs: u64,
    pub kill_after_secs: u64,
    pub max_retries: u32,
    pub task_type: Option<String>,
    pub agent_type: Option<String>,
    pub prompt: Option<String>,
    pub metadata: HashMap<String, String>,
    /// When true, a worker runs this job by calling the registered
    /// `InProcessExecutor` with `args` instead of spawning `command` as an
    /// OS subprocess — `command`/`env`/`cwd`/`kill_after_secs` are then
    /// unused (subprocess-only) but kept for display/back-compat. Defaults
    /// to false via `#[serde(default)]` so every job persisted before this
    /// field existed, and every plain `POST /api/jobs` submission (which
    /// never sets it), keeps spawning a real subprocess exactly as before.
    #[serde(default)]
    pub in_process: bool,
}

impl AgentJob {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: vec![],
            env: vec![],
            cwd: None,
            timeout_secs: 300,
            kill_after_secs: 5,
            max_retries: 3,
            task_type: None,
            agent_type: None,
            prompt: None,
            metadata: HashMap::new(),
            in_process: false,
        }
    }

    /// Marks this job to run via the daemon's registered `InProcessExecutor`
    /// instead of spawning `command` as a subprocess — see the `in_process`
    /// field's doc comment.
    pub fn in_process(mut self) -> Self {
        self.in_process = true;
        self
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        self.env.push((key.into(), val.into()));
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    pub fn max_retries(mut self, n: u32) -> Self {
        self.max_retries = n;
        self
    }

    pub fn agent_type(mut self, t: impl Into<String>) -> Self {
        self.agent_type = Some(t.into());
        self
    }

    pub fn prompt(mut self, p: impl Into<String>) -> Self {
        self.prompt = Some(p.into());
        self
    }

    /// Sets the well-known `dispatch_reason` metadata key, surfaced back on
    /// `JobInfo::dispatch_reason` — e.g. the failing CI check name(s) that
    /// triggered a self-repair dispatch (`run_review_sweep` in
    /// `src/supervisor.rs`), so the dashboard can badge *why* a job ran
    /// instead of only *that* it ran.
    pub fn dispatch_reason(mut self, reason: impl Into<String>) -> Self {
        self.metadata
            .insert("dispatch_reason".to_string(), reason.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobOutput {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
    pub stdout_total_bytes: u64,
    pub stderr_total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobInfo {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub state: JobState,
    pub retries: u32,
    pub max_retries: u32,
    pub error: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub output: Option<JobOutput>,
    pub in_process: bool,
    /// The job's `metadata["dispatch_reason"]`, if set — see
    /// `AgentJob::dispatch_reason`. Purely additive/display: never read to
    /// drive queue behavior, so a job with none is just `None` here.
    pub dispatch_reason: Option<String>,
}
