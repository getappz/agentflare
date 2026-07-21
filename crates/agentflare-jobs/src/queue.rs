use crate::types::{JobInfo, JobOutput, JobState};
use parking_lot::Mutex;
use rusqlite::params;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct Queue {
    conn: Arc<Mutex<rusqlite::Connection>>,
    log_dir: PathBuf,
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
    rusqlite_migration::Migrations::new(vec![rusqlite_migration::M::up(
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
    )])
}

impl Queue {
    pub fn open(path: &Path, log_dir: PathBuf) -> Result<Self, Error> {
        let conn = db_kit::open::open_file(path, &migrations())?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            log_dir,
        })
    }

    pub fn open_memory(log_dir: PathBuf) -> Result<Self, Error> {
        let conn = db_kit::open::open_memory(&migrations())?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            log_dir,
        })
    }

    pub fn log_dir(&self) -> &Path {
        &self.log_dir
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
        Ok(JobInfo {
            id,
            state: JobState::Queued,
            retries: 0,
            max_retries: job.max_retries,
            error: None,
            created_at: now,
            started_at: None,
            finished_at: None,
            output: None,
        })
    }

    pub fn dequeue(&self) -> Result<Option<(String, crate::types::AgentJob)>, Error> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let row: Option<(String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT id, payload FROM agent_jobs
                 WHERE state = 'queued'
                 ORDER BY created_at ASC
                 LIMIT 1",
            )?;
            stmt.query_row([], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
        };
        let (id, payload_json) = match row {
            Some(r) => r,
            None => return Ok(None),
        };
        let now = db_kit::ids::now();
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
                 finished_at = ?6
             WHERE id = ?7",
            params![
                state,
                output.exit_code,
                output.timed_out as i32,
                output.stdout_path.to_string_lossy().as_ref(),
                output.stderr_path.to_string_lossy().as_ref(),
                now,
                id
            ],
        )?;
        Ok(())
    }

    pub fn fail(&self, id: &str, error: &str) -> Result<(), Error> {
        let now = db_kit::ids::now();
        let conn = self.conn.lock();
        let (retries, max_retries): (u32, u32) = conn.query_row(
            "SELECT retries, max_retries FROM agent_jobs WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if retries < max_retries {
            conn.execute(
                "UPDATE agent_jobs
                 SET state = 'queued', retries = retries + 1, error = ?1, started_at = NULL
                 WHERE id = ?2",
                params![error, id],
            )?;
        } else {
            conn.execute(
                "UPDATE agent_jobs
                 SET state = 'failed', error = ?1, finished_at = ?2
                 WHERE id = ?3",
                params![error, now, id],
            )?;
        }
        Ok(())
    }

    pub fn list(&self, state_filter: Option<JobState>) -> Result<Vec<JobInfo>, Error> {
        let conn = self.conn.lock();
        let (where_clause, state_val) = match state_filter {
            Some(ref s) => ("WHERE state = ?1", s.as_sql_state()),
            None => ("", String::new()),
        };
        let sql = format!(
            "SELECT id, state, retries, max_retries, error, created_at, started_at,
                    finished_at, exit_code, timed_out, stdout_log_path, stderr_log_path
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
                    finished_at, exit_code, timed_out, stdout_log_path, stderr_log_path
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

    pub fn cleanup(&self, older_than_secs: i64) -> Result<u64, Error> {
        let cutoff = db_kit::ids::now() - older_than_secs;
        let conn = self.conn.lock();
        let deleted = conn.execute(
            "DELETE FROM agent_jobs WHERE finished_at IS NOT NULL AND finished_at < ?1",
            params![cutoff],
        )?;
        Ok(deleted as u64)
    }
}

fn map_job_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<JobInfo> {
    let state_str: String = r.get(1)?;
    let exit_code: Option<i32> = r.get(8)?;
    let timed_out: bool = r.get::<_, i32>(9)? != 0;
    let stdout_path: Option<String> = r.get(10)?;
    let stderr_path: Option<String> = r.get(11)?;
    Ok(JobInfo {
        id: r.get(0)?,
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
            stdout_total_bytes: 0,
            stderr_total_bytes: 0,
        }),
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
