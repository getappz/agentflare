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
    fn execute(&self, job_id: &str, args: &[String], log: &mut dyn std::io::Write) -> Result<(), String>;
}
