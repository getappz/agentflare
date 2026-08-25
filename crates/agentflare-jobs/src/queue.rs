use crate::types::{JobInfo, JobOutput, JobState};
use parking_lot::{Condvar, Mutex};
use rusqlite::params;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct Queue {
    conn: Arc<Mutex<rusqlite::Connection>>,
    log_dir: PathBuf,
    // Wakes idle workers the moment a job becomes available (fresh enqueue,
    // or a retry going back to 'queued'), instead of them finding out only
    // on their next poll. The bool is a pending-signal flag, not app state:
    // `notify_all()` alone only wakes threads already parked in `wait_for`,
    // so a wake that lands between a worker's dequeue-check and its call to
    // `wait_for_work` would otherwise be lost until the fallback timeout —
    // the flag makes the signal durable across that race by having
    // `wait_for_work` check it (under the same lock) before ever blocking.
    notify: Arc<(Mutex<bool>, Condvar)>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Migration(#[from] rusqlite_migration::Error),
    #[error(transparent)]
    DbKit(#[from] db_kit::open::Error),
    #[error("job not found: {0}")]
    NotFound(String),
    #[error("serialization: {0}")]
    Serialize(#[from] serde_json::Error),
}

pub fn migrations() -> rusqlite_migration::Migrations<'static> {
    rusqlite_migration::Migrations::new(vec![
        rusqlite_migration::M::up(
            "CREATE TABLE IF NOT EXISTS agent_jobs (
                id              TEXT PRIMARY KEY NOT NULL,
                state           TEXT NOT NULL DEFAULT 'queued',
                payload         TEXT NOT NULL,
                retries         INTEGER NOT NULL DEFAULT 0,
                max_retries     INTEGER NOT NULL DEFAULT 3,
                error           TEXT,
                stdout_log_path TEXT,
                stderr_log_path TEXT,
                exit_code       INTEGER,
                timed_out       INTEGER NOT NULL DEFAULT 0,
                created_at      INTEGER NOT NULL,
                started_at      INTEGER,
                finished_at     INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_agent_jobs_state ON agent_jobs(state);
            CREATE INDEX IF NOT EXISTS idx_agent_jobs_created ON agent_jobs(created_at);",
        ),
        rusqlite_migration::M::up(
            "ALTER TABLE agent_jobs ADD COLUMN stdout_bytes INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE agent_jobs ADD COLUMN stderr_bytes INTEGER NOT NULL DEFAULT 0;",
        ),
        rusqlite_migration::M::up("ALTER TABLE agent_jobs ADD COLUMN not_before INTEGER;"),
    ])
}

impl Queue {
    pub fn open(path: &Path, log_dir: PathBuf) -> Result<Self, Error> {
        let conn = db_kit::open::open_file(path, &migrations())?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            log_dir,
            notify: Arc::new((Mutex::new(false), Condvar::new())),
        })
    }

    pub fn open_memory(log_dir: PathBuf) -> Result<Self, Error> {
        let conn = db_kit::open::open_memory(&migrations())?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            log_dir,
            notify: Arc::new((Mutex::new(false), Condvar::new())),
        })
    }

    pub fn log_dir(&self) -> &Path {
        &self.log_dir
    }

    /// Blocks the calling (worker) thread until a job becomes available or
    /// `timeout` elapses, whichever comes first. Checks the pending-signal
    /// flag before blocking, so a `wake_workers` call that landed just
    /// before this one (e.g. between this worker's dequeue-check and this
    /// call) is still observed immediately instead of being lost until
    /// `timeout` — normal pickup is near-instant via `notify`, not the
    /// timeout, in either ordering.
    pub fn wait_for_work(&self, timeout: Duration) {
        let (lock, cvar) = &*self.notify;
        let mut signaled = lock.lock();
        if !*signaled {
            cvar.wait_for(&mut signaled, timeout);
        }
        *signaled = false;
    }

    /// Wakes every thread parked in `wait_for_work` (or the next one to call
    /// it, per the flag above), whether or not there's actually a job for
    /// them — they just re-check `dequeue` either way. Called after
    /// enqueueing/re-queueing a job, and by `WorkerPool::shutdown` so
    /// workers notice a stop request immediately rather than up to
    /// `timeout` later.
    pub fn wake_workers(&self) {
        let (lock, cvar) = &*self.notify;
        let mut signaled = lock.lock();
        *signaled = true;
        cvar.notify_all();
    }

    pub fn enqueue(&self, job: &crate::types::AgentJob) -> Result<JobInfo, Error> {
        let id = db_kit::ids::new_id();
        let now = db_kit::ids::now();
        let payload = serde_json::to_string(job)?;
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO agent_jobs (id, state, payload, max_retries, created_at)
             VALUES (?1, 'queued', ?2, ?3, ?4)",
            params![id, payload, job.max_retries, now],
        )?;
        drop(conn);
        self.wake_workers();
        Ok(JobInfo {
            id,
            command: job.command.clone(),
            args: job.args.clone(),
            state: JobState::Queued,
            retries: 0,
            max_retries: job.max_retries,
            error: None,
            created_at: now,
            started_at: None,
            finished_at: None,
            output: None,
            in_process: job.in_process,
            dispatch_reason: job.metadata.get("dispatch_reason").cloned(),
        })
    }

    pub fn dequeue(&self) -> Result<Option<(String, crate::types::AgentJob)>, Error> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let now = db_kit::ids::now();
        let row: Option<(String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT id, payload FROM agent_jobs
                 WHERE state = 'queued' AND (not_before IS NULL OR not_before <= ?1)
                 ORDER BY created_at ASC
                 LIMIT 1",
            )?;
            stmt.query_row(params![now], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
        };
        let (id, payload_json) = match row {
            Some(r) => r,
            None => return Ok(None),
        };
        tx.execute(
            "UPDATE agent_jobs SET state = 'running', started_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        tx.commit()?;
        let job: crate::types::AgentJob = serde_json::from_str(&payload_json)?;
        Ok(Some((id, job)))
    }

    pub fn complete(&self, id: &str, output: &JobOutput, success: bool) -> Result<(), Error> {
        let now = db_kit::ids::now();
        let state = if success { "exited" } else { "failed" };
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE agent_jobs
             SET state = ?1,
                 exit_code = ?2,
                 timed_out = ?3,
                 stdout_log_path = ?4,
                 stderr_log_path = ?5,
                 stdout_bytes = ?6,
                 stderr_bytes = ?7,
                 finished_at = ?8
             WHERE id = ?9",
            params![
                state,
                output.exit_code,
                output.timed_out as i32,
                output.stdout_path.to_string_lossy().as_ref(),
                output.stderr_path.to_string_lossy().as_ref(),
                output.stdout_total_bytes as i64,
                output.stderr_total_bytes as i64,
                now,
                id
            ],
        )?;
        Ok(())
    }

    /// Persists a job's log paths/byte counts independently of `state` —
    /// for `run_in_process`'s failure/timeout paths, which already wrote a
    /// real stdout log before the executor errored out or the timeout fired
    /// and go on to call `fail` (not `complete`, since `fail` alone carries
    /// the retry-vs-terminal logic), so without this the log a job actually
    /// produced was silently discarded and `stdout_log_path` stayed NULL
    /// forever even though the file exists on disk.
    pub fn record_output(
        &self,
        id: &str,
        stdout_path: &Path,
        stderr_path: &Path,
        stdout_bytes: u64,
        stderr_bytes: u64,
    ) -> Result<(), Error> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE agent_jobs
             SET stdout_log_path = ?1,
                 stderr_log_path = ?2,
                 stdout_bytes = ?3,
                 stderr_bytes = ?4
             WHERE id = ?5",
            params![
                stdout_path.to_string_lossy().as_ref(),
                stderr_path.to_string_lossy().as_ref(),
                stdout_bytes as i64,
                stderr_bytes as i64,
                id
            ],
        )?;
        Ok(())
    }

    /// Returns `true` when this failure was terminal (retries exhausted, or
    /// `fatal` short-circuited them, row left `state = 'failed'`), `false`
    /// when it went back to `queued` for another attempt — callers that need
    /// to react to a job's *permanent* failure (e.g.
    /// `worker::run_in_process`'s terminal-failure hook) use this instead of
    /// re-deriving it from a follow-up `get`.
    ///
    /// `fatal` skips the retry budget entirely and marks the job `failed`
    /// regardless of how many retries remain — for a failure classified as
    /// structural (see `JobFailure::fatal`'s doc comment), retrying would
    /// just fail identically, so there's no point spending the normal
    /// transient-failure backoff budget before the terminal-failure recovery
    /// hook gets a chance to run.
    pub fn fail(
        &self,
        id: &str,
        error: &str,
        retry_after_secs: Option<u64>,
        fatal: bool,
    ) -> Result<bool, Error> {
        let now = db_kit::ids::now();
        let conn = self.conn.lock();
        let (retries, max_retries): (u32, u32) = conn.query_row(
            "SELECT retries, max_retries FROM agent_jobs WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let retried = !fatal && retries < max_retries;
        if retried {
            let not_before = retry_after_secs.map(|s| now + s as i64);
            conn.execute(
                "UPDATE agent_jobs
                 SET state = 'queued', retries = retries + 1, error = ?1, started_at = NULL, not_before = ?2
                 WHERE id = ?3",
                params![error, not_before, id],
            )?;
        } else {
            conn.execute(
                "UPDATE agent_jobs
                 SET state = 'failed', error = ?1, finished_at = ?2
                 WHERE id = ?3",
                params![error, now, id],
            )?;
        }
        drop(conn);
        // A retry goes back to 'queued' — wake workers so it's picked up
        // promptly instead of waiting out the fallback poll interval. Waking
        // them even when `not_before` is in the future is harmless (their
        // next `dequeue` simply finds nothing and re-parks) and keeps this
        // function simple; the fallback poll in `worker_loop` is the real
        // backstop for the delayed case regardless.
        if retried {
            self.wake_workers();
        }
        Ok(!retried)
    }

    /// Sweeps every `agent_jobs` row still `state = 'running'` and marks it
    /// terminal (`failed`) with an explanatory error and `finished_at` set.
    ///
    /// A freshly opened `Queue` can never have legitimately produced a
    /// `running` row itself — nothing has dequeued anything yet — so any row
    /// already in that state was left mid-flight by a previous process's
    /// death (e.g. a daemon restart killing the worker thread and its
    /// `run_in_process` timeout watchdog before either could finish it).
    /// Must be called exactly once, right after `open`/`open_memory` and
    /// before anything starts dequeuing (see `dashboard::server::run`) —
    /// calling it again later would wrongly fail jobs this same process is
    /// legitimately running.
    ///
    /// Returns the reconciled `(id, AgentJob)` pairs, parsed from each row's
    /// own payload, so the caller — which knows how to release a work item's
    /// claim; this crate doesn't — can release whatever claim each job's
    /// item held. A row whose payload fails to parse is still marked failed
    /// but omitted from the returned list (nothing to release).
    pub fn reconcile_orphaned_running(
        &self,
    ) -> Result<Vec<(String, crate::types::AgentJob)>, Error> {
        const ORPHAN_ERROR: &str = "orphaned by daemon restart";
        let now = db_kit::ids::now();
        let conn = self.conn.lock();
        let rows: Vec<(String, String)> = {
            let mut stmt =
                conn.prepare("SELECT id, payload FROM agent_jobs WHERE state = 'running'")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (id, _) in &rows {
            // Both the subprocess (`Supervisor`) and in-process
            // (`run_in_process`) paths name their log files `{id}.stdout`/
            // `{id}.stderr` under `log_dir()` and may have already written
            // real output before the daemon died mid-job — without this,
            // that log survives on disk but the row's `stdout_log_path`
            // stays NULL forever, so it's unreachable via `JobInfo`/the
            // dashboard even though the file is right there.
            let stdout_path = self.log_dir.join(format!("{id}.stdout"));
            let stderr_path = self.log_dir.join(format!("{id}.stderr"));
            let stdout_bytes = std::fs::metadata(&stdout_path).map(|m| m.len()).unwrap_or(0);
            let stderr_bytes = std::fs::metadata(&stderr_path).map(|m| m.len()).unwrap_or(0);
            conn.execute(
                "UPDATE agent_jobs
                 SET state = 'failed', error = ?1, finished_at = ?2,
                     stdout_log_path = ?3, stderr_log_path = ?4,
                     stdout_bytes = ?5, stderr_bytes = ?6
                 WHERE id = ?7",
                params![
                    ORPHAN_ERROR,
                    now,
                    stdout_path.to_string_lossy().as_ref(),
                    stderr_path.to_string_lossy().as_ref(),
                    stdout_bytes as i64,
                    stderr_bytes as i64,
                    id
                ],
            )?;
        }
        Ok(rows
            .into_iter()
            .filter_map(|(id, payload)| {
                serde_json::from_str::<crate::types::AgentJob>(&payload)
                    .ok()
                    .map(|job| (id, job))
            })
            .collect())
    }

    pub fn list(&self, state_filter: Option<JobState>) -> Result<Vec<JobInfo>, Error> {
        let conn = self.conn.lock();
        let (where_clause, state_val) = match state_filter {
            Some(ref s) => ("WHERE state = ?1", s.as_sql_state()),
            None => ("", String::new()),
        };
        let sql = format!(
            "SELECT id, state, retries, max_retries, error, created_at, started_at,
                    finished_at, exit_code, timed_out, stdout_log_path, stderr_log_path,
                    stdout_bytes, stderr_bytes, payload
             FROM agent_jobs {where_clause}
             ORDER BY created_at DESC
             LIMIT 100"
        );
        let mut stmt = conn.prepare(&sql)?;

        let jobs: Vec<JobInfo> = if state_filter.is_some() {
            stmt.query_map(params![state_val], map_job_row)?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map([], map_job_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(jobs)
    }

    pub fn get(&self, id: &str) -> Result<JobInfo, Error> {
        let conn = self.conn.lock();
        let row = conn.query_row(
            "SELECT id, state, retries, max_retries, error, created_at, started_at,
                    finished_at, exit_code, timed_out, stdout_log_path, stderr_log_path,
                    stdout_bytes, stderr_bytes, payload
             FROM agent_jobs WHERE id = ?1",
            params![id],
            map_job_row,
        )?;
        Ok(row)
    }

    pub fn cancel(&self, id: &str) -> Result<(), Error> {
        let now = db_kit::ids::now();
        let conn = self.conn.lock();
        let affected = conn.execute(
            "UPDATE agent_jobs SET state = 'killed', finished_at = ?1
             WHERE id = ?2 AND state = 'queued'",
            params![now, id],
        )?;
        if affected == 0 {
            return Err(Error::NotFound(id.to_string()));
        }
        Ok(())
    }

    /// Deletes finished jobs older than `older_than_secs`, including their
    /// stdout/stderr log files on disk — those aren't tracked anywhere else,
    /// so leaving them behind here would mean disk usage grows forever even
    /// as the DB rows are reclaimed.
    pub fn cleanup(&self, older_than_secs: i64) -> Result<u64, Error> {
        let cutoff = db_kit::ids::now() - older_than_secs;
        let conn = self.conn.lock();
        let log_paths: Vec<(Option<String>, Option<String>)> = {
            let mut stmt = conn.prepare(
                "SELECT stdout_log_path, stderr_log_path FROM agent_jobs
                 WHERE finished_at IS NOT NULL AND finished_at < ?1",
            )?;
            stmt.query_map(params![cutoff], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let deleted = conn.execute(
            "DELETE FROM agent_jobs WHERE finished_at IS NOT NULL AND finished_at < ?1",
            params![cutoff],
        )?;
        drop(conn);
        for (stdout_path, stderr_path) in log_paths {
            if let Some(p) = stdout_path {
                let _ = std::fs::remove_file(p);
            }
            if let Some(p) = stderr_path {
                let _ = std::fs::remove_file(p);
            }
        }
        Ok(deleted as u64)
    }
}

fn map_job_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<JobInfo> {
    let state_str: String = r.get(1)?;
    let exit_code: Option<i32> = r.get(8)?;
    let timed_out: bool = r.get::<_, i32>(9)? != 0;
    let stdout_path: Option<String> = r.get(10)?;
    let stderr_path: Option<String> = r.get(11)?;
    let stdout_bytes: i64 = r.get(12)?;
    let stderr_bytes: i64 = r.get(13)?;
    let payload_json: String = r.get(14)?;
    // `payload` is our own `serde_json::to_string(&AgentJob)` from `enqueue`,
    // so a parse failure here would mean on-disk corruption, not bad input —
    // fall back to an empty command/args rather than failing the whole read.
    let (command, args, in_process, dispatch_reason) =
        serde_json::from_str::<crate::types::AgentJob>(&payload_json)
            .map(|job| {
                (
                    job.command,
                    job.args,
                    job.in_process,
                    job.metadata.get("dispatch_reason").cloned(),
                )
            })
            .unwrap_or_default();
    Ok(JobInfo {
        id: r.get(0)?,
        command,
        args,
        state: match state_str.as_str() {
            "running" => JobState::Running,
            "exited" => JobState::Exited,
            "killed" => JobState::Killed,
            "failed" => JobState::Failed,
            _ => JobState::Queued,
        },
        retries: r.get(2)?,
        max_retries: r.get(3)?,
        error: r.get(4)?,
        created_at: r.get(5)?,
        started_at: r.get(6)?,
        finished_at: r.get(7)?,
        output: exit_code.map(|code| JobOutput {
            exit_code: Some(code),
            timed_out,
            stdout_path: stdout_path.map(Into::into).unwrap_or_default(),
            stderr_path: stderr_path.map(Into::into).unwrap_or_default(),
            stdout_total_bytes: stdout_bytes as u64,
            stderr_total_bytes: stderr_bytes as u64,
        }),
        in_process,
        dispatch_reason,
    })
}

trait AsSqlState {
    fn as_sql_state(&self) -> String;
}

impl AsSqlState for JobState {
    fn as_sql_state(&self) -> String {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Exited => "exited",
            JobState::Killed => "killed",
            JobState::Failed => "failed",
        }
        .to_string()
    }
}

trait OptionalExt<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalExt<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AgentJob;

    fn test_queue() -> (Queue, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let queue = Queue::open_memory(dir.path().join("logs")).unwrap();
        (queue, dir)
    }

    // A `run_in_process` failure/timeout still writes a real stdout log
    // before calling `fail` (which alone doesn't touch the log columns) --
    // `record_output` is how that log's path/size gets persisted so it
    // isn't orphaned on disk with a `NULL` `stdout_log_path`, regardless of
    // whether the row ends up 'queued' (retried) or 'failed' (terminal).
    #[test]
    fn record_output_persists_log_paths_independently_of_fail() {
        let (queue, _dir) = test_queue();
        let job = AgentJob::new("true").max_retries(0);
        let info = queue.enqueue(&job).unwrap();
        queue.dequeue().unwrap();

        queue
            .record_output(
                &info.id,
                Path::new("/tmp/job.stdout"),
                Path::new("/tmp/job.stderr"),
                42,
                7,
            )
            .unwrap();
        queue.fail(&info.id, "boom", None, true).unwrap();

        let conn = queue.conn.lock();
        let (stdout_path, stderr_path, stdout_bytes, stderr_bytes): (String, String, i64, i64) =
            conn.query_row(
                "SELECT stdout_log_path, stderr_log_path, stdout_bytes, stderr_bytes
                 FROM agent_jobs WHERE id = ?1",
                params![info.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(stdout_path, "/tmp/job.stdout");
        assert_eq!(stderr_path, "/tmp/job.stderr");
        assert_eq!(stdout_bytes, 42);
        assert_eq!(stderr_bytes, 7);
    }

    #[test]
    fn fail_without_retry_delay_requeues_immediately() {
        let (queue, _dir) = test_queue();
        let job = AgentJob::new("true").max_retries(1);
        let info = queue.enqueue(&job).unwrap();
        queue.dequeue().unwrap();
        queue.fail(&info.id, "boom", None, false).unwrap();
        let (id, _) = queue
            .dequeue()
            .unwrap()
            .expect("retried job should be immediately eligible");
        assert_eq!(id, info.id);
    }

    #[test]
    fn fail_with_retry_delay_hides_the_job_until_not_before_passes() {
        let (queue, _dir) = test_queue();
        let job = AgentJob::new("true").max_retries(1);
        let info = queue.enqueue(&job).unwrap();
        queue.dequeue().unwrap();
        queue
            .fail(&info.id, "rate limited", Some(3600), false)
            .unwrap();

        assert!(
            queue.dequeue().unwrap().is_none(),
            "a job with a future not_before must not be dequeued yet"
        );

        // Simulate the delay having elapsed, without a real sleep.
        {
            let conn = queue.conn.lock();
            conn.execute(
                "UPDATE agent_jobs SET not_before = 0 WHERE id = ?1",
                params![info.id],
            )
            .unwrap();
        }
        let (id, _) = queue
            .dequeue()
            .unwrap()
            .expect("job should be eligible once not_before has passed");
        assert_eq!(id, info.id);
    }

    #[test]
    fn reconcile_orphaned_running_marks_stale_running_jobs_failed_and_returns_them() {
        let (queue, _dir) = test_queue();
        let job = AgentJob::new("agentflare-work")
            .args(["item-1", "claude-code"])
            .in_process();
        let info = queue.enqueue(&job).unwrap();
        queue.dequeue().unwrap(); // simulate a prior process having claimed it

        let reconciled = queue.reconcile_orphaned_running().unwrap();
        assert_eq!(reconciled.len(), 1);
        assert_eq!(reconciled[0].0, info.id);
        assert_eq!(reconciled[0].1.args, vec!["item-1", "claude-code"]);

        let stored = queue.get(&info.id).unwrap();
        assert_eq!(stored.state, JobState::Failed);
        assert!(stored.finished_at.is_some());
        assert_eq!(stored.error.as_deref(), Some("orphaned by daemon restart"));
    }

    // A job killed mid-run by a daemon restart may already have written real
    // progress to its `{id}.stdout` file before the process died -- without
    // persisting that path/size here, the log survives on disk but is
    // unreachable (no DB row points at it) and `cleanup` can never find it
    // to delete it either, so it leaks forever. Reproduced live: 599 failed
    // rows with an empty `stdout_log_path` alongside real `.stdout` files on
    // disk, all originating from this exact path.
    #[test]
    fn reconcile_orphaned_running_persists_the_stdout_log_already_written_to_disk() {
        let (queue, _dir) = test_queue();
        let job = AgentJob::new("agentflare-work")
            .args(["item-1", "claude-code"])
            .in_process();
        let info = queue.enqueue(&job).unwrap();
        queue.dequeue().unwrap();

        std::fs::create_dir_all(queue.log_dir()).unwrap();
        let stdout_path = queue.log_dir().join(format!("{}.stdout", info.id));
        std::fs::write(&stdout_path, "claimed: item-1\nworktree: ...\n").unwrap();

        queue.reconcile_orphaned_running().unwrap();

        let conn = queue.conn.lock();
        let (path, bytes): (String, i64) = conn
            .query_row(
                "SELECT stdout_log_path, stdout_bytes FROM agent_jobs WHERE id = ?1",
                params![info.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(path, stdout_path.to_string_lossy());
        assert_eq!(bytes, 30);
    }

    #[test]
    fn reconcile_orphaned_running_leaves_terminal_and_queued_jobs_untouched() {
        let (queue, _dir) = test_queue();
        // Already terminal -- must not be double-processed.
        let failed_job = AgentJob::new("true").max_retries(0);
        let failed_info = queue.enqueue(&failed_job).unwrap();
        queue.dequeue().unwrap();
        queue.fail(&failed_info.id, "boom", None, false).unwrap();

        // Never dequeued -- not orphaned, must stay queued.
        let queued_info = queue.enqueue(&AgentJob::new("true")).unwrap();

        let reconciled = queue.reconcile_orphaned_running().unwrap();
        assert!(reconciled.is_empty());

        let failed_stored = queue.get(&failed_info.id).unwrap();
        assert_eq!(failed_stored.state, JobState::Failed);
        assert_eq!(failed_stored.error.as_deref(), Some("boom"));
        assert_eq!(queue.get(&queued_info.id).unwrap().state, JobState::Queued);
    }

    #[test]
    fn reconcile_orphaned_running_does_not_touch_a_job_this_process_dequeues_after_the_call() {
        let (queue, _dir) = test_queue();
        let info = queue.enqueue(&AgentJob::new("true")).unwrap();

        // Reconciliation runs before any dequeuing starts (as daemon startup
        // does) -- nothing is running yet, so this is a no-op.
        assert!(queue.reconcile_orphaned_running().unwrap().is_empty());

        // This process now dequeues it itself; reconcile is never called
        // again for the life of this process, so this row stays running.
        queue.dequeue().unwrap();
        assert_eq!(queue.get(&info.id).unwrap().state, JobState::Running);
    }

    #[test]
    fn dispatch_reason_round_trips_through_enqueue_list_and_get() {
        let (queue, _dir) = test_queue();
        let job = AgentJob::new("agentflare-work")
            .args(["item-1", "claude-code"])
            .in_process()
            .dispatch_reason("self-repair: Validate PR title");
        let info = queue.enqueue(&job).unwrap();
        assert_eq!(
            info.dispatch_reason.as_deref(),
            Some("self-repair: Validate PR title")
        );

        let listed = queue.list(None).unwrap();
        assert_eq!(
            listed[0].dispatch_reason.as_deref(),
            Some("self-repair: Validate PR title")
        );

        let got = queue.get(&info.id).unwrap();
        assert_eq!(
            got.dispatch_reason.as_deref(),
            Some("self-repair: Validate PR title")
        );

        // A plain dispatch (no reason set) must stay `None`, not e.g. `""`.
        let plain = queue.enqueue(&AgentJob::new("true")).unwrap();
        assert_eq!(plain.dispatch_reason, None);
        assert_eq!(queue.get(&plain.id).unwrap().dispatch_reason, None);
    }

    #[test]
    fn fail_past_max_retries_marks_failed_regardless_of_retry_delay() {
        let (queue, _dir) = test_queue();
        let job = AgentJob::new("true").max_retries(0);
        let info = queue.enqueue(&job).unwrap();
        queue.dequeue().unwrap();
        queue.fail(&info.id, "boom", Some(60), false).unwrap();
        let jobs = queue.list(Some(JobState::Failed)).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, info.id);
    }

    #[test]
    fn fail_fatal_marks_failed_immediately_even_with_retries_remaining() {
        // Mirrors a structural setup failure (e.g. `agentflare work`'s "no
        // worktree was created" path, item #467): the underlying cause can't
        // change between attempts, so it must reach terminal `state =
        // 'failed'` on the very first failure instead of consuming the
        // normal transient-failure retry budget.
        let (queue, _dir) = test_queue();
        let job = AgentJob::new("true").max_retries(3);
        let info = queue.enqueue(&job).unwrap();
        queue.dequeue().unwrap();

        let terminal = queue.fail(&info.id, "boom", None, true).unwrap();

        assert!(
            terminal,
            "a fatal failure must be terminal even though retries remain"
        );
        let stored = queue.get(&info.id).unwrap();
        assert_eq!(stored.state, JobState::Failed);
        assert_eq!(
            stored.retries, 0,
            "retries must not be incremented on a fatal failure"
        );
    }
}
