//! Mutation events: generated write methods report `Created/Updated/Deleted/Restored/
//! Upserted` to a process-wide sink after the statement succeeds.
//!
//! "Succeeded" means the statement returned without error. When the caller passes a
//! transaction executor the surrounding transaction may still roll back, so use
//! your own outbox for commit-gated delivery; methods that open their own
//! transaction (`update_many`) emit only after that transaction commits.

use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationKind {
    Created,
    Updated,
    /// Hard delete or soft delete (`deleted_at` set).
    Deleted,
    /// `deleted_at` cleared.
    Restored,
    /// `upsert_*` — insert vs update is not distinguished.
    Upserted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationEvent {
    /// The entity's table name.
    pub object: &'static str,
    pub kind: MutationKind,
    /// Primary key as text. `None` for filter-based bulk writes (`update_where`,
    /// `soft_delete_where`, `restore_where`), which only know a row count.
    pub id: Option<String>,
}

type Sink = Arc<dyn Fn(&MutationEvent) + Send + Sync>;

static SINK: RwLock<Option<Sink>> = RwLock::new(None);

/// Installs the process-wide event sink, replacing any previous one.
pub fn set_event_sink(sink: impl Fn(&MutationEvent) + Send + Sync + 'static) {
    *SINK.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(sink));
}

pub fn clear_event_sink() {
    *SINK.write().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Used by generated code.
pub fn emit(object: &'static str, kind: MutationKind, id: Option<String>) {
    let sink = SINK.read().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(sink) = sink {
        sink(&MutationEvent { object, kind, id });
    }
}
