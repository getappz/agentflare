/// Lets a worker run certain jobs by calling application code directly,
/// in-process, instead of spawning a fresh OS subprocess for `job.command`
/// (see `AgentJob::in_process`/`in_process()`). Defined in this crate,
/// implemented by the caller: this crate's `Queue`/`Supervisor`/`WorkerPool`
/// stay generic and never depend upward on whatever binary embeds them — the
/// binary implements this trait for its own job kind(s) and hands an
/// instance to `WorkerPool::with_executor`.
pub trait InProcessExecutor: Send + Sync {
    /// Runs one job's `args`. `log` should receive the same kind of
    /// progress output a subprocess would have written to stdout — a worker
    /// writes it to that job's own `{id}.stdout` log file, the same file
    /// name/location `Supervisor::spawn` uses for subprocess jobs, so
    /// existing log-tailing (e.g. the dashboard's `/api/jobs/:id/stream`)
    /// keeps working unchanged for in-process jobs too.
    fn execute(
        &self,
        job_id: &str,
        args: &[String],
        log: &mut dyn std::io::Write,
    ) -> Result<(), JobFailure>;
}

/// What went wrong, plus an optional hint for how long to wait before this
/// job (if it still has retries left) becomes eligible again. Set by a
/// caller that classifies the failure as transient/rate-limit shaped (see
/// the main binary's `auth_runner`/`auth_db` — this crate stays agnostic to
/// *why* a delay is warranted, it just carries the number through to
/// `Queue::fail`). Left `None` for anything else, which keeps `Queue::fail`'s
/// existing instant-requeue behavior.
pub struct JobFailure {
    pub message: String,
    pub retry_after_secs: Option<u64>,
    /// Set by a caller that knows the underlying cause can't change between
    /// attempts (e.g. `agentflare work`'s "claim succeeded but no worktree
    /// was created" path — a broken git worktree registration doesn't heal
    /// itself by retrying against it again). `true` sends the job straight
    /// to terminal `state = 'failed'` in `Queue::fail`, regardless of how
    /// many retries remain, instead of consuming the normal
    /// transient-failure retry budget before the existing terminal-failure
    /// recovery hook can run.
    pub fatal: bool,
}

impl From<String> for JobFailure {
    fn from(message: String) -> Self {
        JobFailure {
            message,
            retry_after_secs: None,
            fatal: false,
        }
    }
}

impl From<&str> for JobFailure {
    fn from(message: &str) -> Self {
        JobFailure {
            message: message.to_string(),
            retry_after_secs: None,
            fatal: false,
        }
    }
}
