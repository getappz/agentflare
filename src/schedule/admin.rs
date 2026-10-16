//! CLI/MCP-facing schedule administration over [`ScheduleStore`]. Enqueues
//! and edits rows only — the daemon's `Scheduler` owns execution.

use std::path::Path;

use serde::Serialize;

use super::store::{CreateInput, UpdateInput};
use super::{
    ScheduleRow, ScheduleStore, canonical_repo_root, validate_every_secs, validate_params,
};

/// JSON/display shape of a schedule row, with the grace the scheduler applies.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ScheduleView {
    pub id: String,
    pub name: String,
    pub repo_root: String,
    pub workflow_name: String,
    pub params: Option<serde_json::Value>,
    pub every_secs: i64,
    pub grace_secs: Option<i64>,
    pub overlap: String,
    pub enabled: bool,
    pub next_fire_at: i64,
    pub last_fire_at: Option<i64>,
    pub frozen_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub effective_grace_secs: i64,
}

impl From<ScheduleRow> for ScheduleView {
    fn from(r: ScheduleRow) -> Self {
        Self {
            effective_grace_secs: r.effective_grace_secs(),
            params: r
                .params_raw
                .as_deref()
                .and_then(|p| serde_json::from_str(p).ok()),
            id: r.id,
            name: r.name,
            repo_root: r.repo_root,
            workflow_name: r.workflow_name,
            every_secs: r.every_secs,
            grace_secs: r.grace_secs,
            overlap: r.overlap,
            enabled: r.enabled,
            next_fire_at: r.next_fire_at,
            last_fire_at: r.last_fire_at,
            frozen_error: r.frozen_error,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct OccurrenceView {
    pub id: String,
    pub slot: i64,
    pub trigger: String,
    pub status: String,
    pub run_id: Option<String>,
    pub error: Option<String>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
}

pub(crate) struct NewSchedule {
    pub name: String,
    pub repo_root: String,
    pub workflow_name: String,
    pub params: Option<serde_json::Value>,
    pub every_secs: i64,
    pub grace_secs: Option<i64>,
    pub overlap: String,
}

/// `Some` fields replace the stored value; an empty patch is rejected.
#[derive(Default)]
pub(crate) struct SchedulePatch {
    pub workflow_name: Option<String>,
    pub params: Option<serde_json::Value>,
    pub every_secs: Option<i64>,
    pub grace_secs: Option<i64>,
    pub overlap: Option<String>,
}

/// `every` duration; rejects intervals below the 60 s tick.
pub(crate) fn parse_interval(text: &str) -> Result<i64, String> {
    let secs = parse_grace(text)?;
    validate_every_secs(secs)?;
    Ok(secs)
}

pub(crate) fn parse_grace(text: &str) -> Result<i64, String> {
    let d = crate::cli::parse_duration(text)?;
    i64::try_from(d.as_secs()).map_err(|_| format!("invalid duration '{text}' (overflow)"))
}

pub(crate) fn parse_params(text: Option<&str>) -> Result<Option<serde_json::Value>, String> {
    let Some(text) = text.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("invalid params JSON: {e}"))?;
    validate_params(&value)?;
    Ok(Some(value))
}

pub(crate) async fn create(db: &Path, s: NewSchedule, now: i64) -> Result<ScheduleView, String> {
    let row = ScheduleStore::open(db)?
        .create_schedule(
            CreateInput {
                name: s.name,
                repo_root: s.repo_root,
                workflow_name: s.workflow_name,
                params: s.params,
                every_secs: s.every_secs,
                grace_secs: s.grace_secs,
                overlap: s.overlap,
            },
            now,
        )
        .await?;
    Ok(row.into())
}

/// `repo_root = None` lists every repository.
pub(crate) async fn list(db: &Path, repo_root: Option<&str>) -> Result<Vec<ScheduleView>, String> {
    let store = ScheduleStore::open(db)?;
    let rows = match repo_root {
        Some(root) => store.list_for_repo(&canonical_repo_root(root)?).await?,
        None => store.list_all().await?,
    };
    Ok(rows.into_iter().map(Into::into).collect())
}

/// A schedule id resolves anywhere; a name resolves within `repo_root`.
async fn resolve(
    store: &ScheduleStore,
    repo_root: Option<&str>,
    token: &str,
) -> Result<ScheduleRow, String> {
    if let Some(row) = store.get_by_id(token).await? {
        return Ok(row);
    }
    let root = repo_root.ok_or_else(|| "not inside a git repository".to_string())?;
    store
        .get_by_name(&canonical_repo_root(root)?, token)
        .await?
        .ok_or_else(|| format!("no schedule '{token}'"))
}

pub(crate) async fn show(
    db: &Path,
    repo_root: Option<&str>,
    token: &str,
) -> Result<ScheduleView, String> {
    resolve(&ScheduleStore::open(db)?, repo_root, token)
        .await
        .map(Into::into)
}

pub(crate) async fn update(
    db: &Path,
    repo_root: Option<&str>,
    token: &str,
    p: SchedulePatch,
    now: i64,
) -> Result<ScheduleView, String> {
    if p.workflow_name.is_none()
        && p.params.is_none()
        && p.every_secs.is_none()
        && p.grace_secs.is_none()
        && p.overlap.is_none()
    {
        return Err("nothing to update".into());
    }
    let store = ScheduleStore::open(db)?;
    let id = resolve(&store, repo_root, token).await?.id;
    let row = store
        .update_schedule(
            &id,
            UpdateInput {
                workflow_name: p.workflow_name,
                params: p.params,
                every_secs: p.every_secs,
                grace_secs: p.grace_secs.map(Some),
                overlap: p.overlap,
            },
            now,
        )
        .await?;
    Ok(row.into())
}

pub(crate) async fn set_enabled(
    db: &Path,
    repo_root: Option<&str>,
    token: &str,
    enabled: bool,
    now: i64,
) -> Result<ScheduleView, String> {
    let store = ScheduleStore::open(db)?;
    let id = resolve(&store, repo_root, token).await?.id;
    store.set_enabled(&id, enabled, now).await.map(Into::into)
}

pub(crate) async fn remove(db: &Path, repo_root: Option<&str>, token: &str) -> Result<(), String> {
    let store = ScheduleStore::open(db)?;
    let id = resolve(&store, repo_root, token).await?.id;
    store.remove_schedule(&id).await.map(|_| ())
}

/// Queue a manual occurrence; never starts a workflow.
pub(crate) async fn enqueue_manual(
    db: &Path,
    repo_root: Option<&str>,
    token: &str,
    now: i64,
) -> Result<super::FireReceipt, String> {
    let store = ScheduleStore::open(db)?;
    let id = resolve(&store, repo_root, token).await?.id;
    store.enqueue_manual(&id, now).await
}

pub(crate) async fn runs(
    db: &Path,
    repo_root: Option<&str>,
    token: &str,
    limit: Option<i64>,
) -> Result<Vec<OccurrenceView>, String> {
    let limit = match limit {
        Some(n) if n < 0 => return Err("limit must be >= 0".into()),
        Some(0) => return Ok(Vec::new()),
        Some(n) => n,
        None => -1,
    };
    let store = ScheduleStore::open(db)?;
    let id = resolve(&store, repo_root, token).await?.id;
    Ok(store
        .occurrences_for_schedule(&id, limit)
        .await?
        .into_iter()
        .map(|o| OccurrenceView {
            id: o.id,
            slot: o.slot,
            trigger: o.trigger,
            status: o.status,
            run_id: o.run_id,
            error: o.error,
            created_at: o.created_at,
            finished_at: o.finished_at,
        })
        .collect())
}
