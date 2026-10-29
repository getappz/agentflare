//! Best-effort local writes with a bounded exit flush and durable drop accounting.
use super::writer::Writer;
use super::{capture, shadow};
use serde::{Serialize, de::DeserializeOwned};
use std::path::{Path, PathBuf};
use std::sync::{
    OnceLock,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

pub const LEDGER_CAP: u64 = 5 * 1024 * 1024;
static WRITER: OnceLock<Option<Writer>> = OnceLock::new();
static START_FAILURES: AtomicU64 = AtomicU64::new(0);

pub(super) enum Work {
    Decision(Box<shadow::Row>),
    Capture(Box<capture::Row>),
}

#[derive(Clone)]
pub(super) struct Pending {
    pub capture: bool,
    pub id: String,
    pub bytes: std::sync::Arc<Vec<u8>>,
}

impl Work {
    pub fn pending(&mut self) -> Pending {
        let (capture, id) = match self {
            Work::Decision(row) => (false, &mut row.decision_id),
            Work::Capture(row) => (true, &mut row.decision_id),
        };
        Pending {
            capture,
            id: id.get_or_insert_with(super::tracking::decision_id).clone(),
            bytes: Default::default(),
        }
    }
}

pub(super) fn drop_record(count: u64, pending: &[Pending]) -> serde_json::Value {
    serde_json::json!({
        "record_type": "dropped",
        "count": count + pending.len() as u64,
        "decision_ids": pending.iter().filter(|p| !p.capture).map(|p| &p.id).collect::<Vec<_>>(),
        "capture_ids": pending.iter().filter(|p| p.capture).map(|p| &p.id).collect::<Vec<_>>(),
    })
}

pub(super) fn persist_drops(path: &Path, counter: &AtomicU64, pending: &[Pending]) -> bool {
    let count = counter.swap(0, Ordering::AcqRel);
    if count == 0 && pending.is_empty() {
        return true;
    }
    let record = drop_record(count, pending);
    if append(path, &record, LEDGER_CAP).is_ok() {
        true
    } else {
        counter.fetch_add(count, Ordering::Relaxed);
        false
    }
}

fn writer() -> Option<&'static Writer> {
    WRITER
        .get_or_init(|| Writer::new(shadow::log_path(), capture::dataset_path()).ok())
        .as_ref()
}

fn enqueue(work: Work) {
    if let Some(w) = writer() {
        w.enqueue(work);
    } else {
        START_FAILURES.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn record(row: shadow::Row) {
    enqueue(Work::Decision(Box::new(row)));
}
pub fn capture(row: capture::Row) {
    enqueue(Work::Capture(Box::new(row)));
}
pub fn pending_drops() -> u64 {
    START_FAILURES.load(Ordering::Relaxed)
        + WRITER
            .get()
            .and_then(Option::as_ref)
            .map_or(0, Writer::pending_drops)
}

/// Read-after-write barrier; failed startup is reported even without a writer.
pub fn flush(timeout: Duration) -> bool {
    let flushed = WRITER
        .get()
        .and_then(Option::as_ref)
        .is_none_or(|w| w.flush(timeout));
    (START_FAILURES.load(Ordering::Relaxed) == 0
        || persist_drops(&shadow::log_path(), &START_FAILURES, &[]))
        && flushed
}

/// Bounded flush for ordinary returns and direct exit paths.
pub fn shutdown() {
    if let Some(w) = WRITER.get().and_then(Option::as_ref) {
        w.shutdown();
    }
    if START_FAILURES.load(Ordering::Relaxed) > 0 {
        let _ = super::recovery::handoff(
            &shadow::log_path(),
            &capture::dataset_path(),
            &START_FAILURES,
            &[],
        );
    }
}

pub fn exit(code: i32) -> ! {
    shutdown();
    std::process::exit(code)
}

pub(crate) fn append(path: &Path, value: &impl Serialize, cap: u64) -> std::io::Result<()> {
    capture::append(path, value, cap)
}

pub fn load<T: DeserializeOwned>(path: &Path) -> Vec<T> {
    [path.with_extension("jsonl.1"), path.to_path_buf()]
        .iter()
        .flat_map(|p| {
            std::fs::read_to_string(p)
                .unwrap_or_default()
                .lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect::<Vec<T>>()
        })
        .collect()
}

pub fn bytes(path: &Path) -> u64 {
    [path.to_path_buf(), path.with_extension("jsonl.1")]
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "storage_lifecycle_tests.rs"]
mod lifecycle_tests;
