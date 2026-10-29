//! Best-effort local writer; accepted IPC bytes outlive the caller process.
use super::writer_process::Writer;
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
pub fn prepare() {
    let _ = writer();
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

/// Explicit read-after-write barrier for consumers, never used for hook exit.
pub fn flush(timeout: Duration) -> bool {
    WRITER
        .get()
        .and_then(Option::as_ref)
        .is_none_or(|w| w.flush(timeout))
}

/// Closing IPC hands EOF to the helper; no disk wait on the hook's exit path.
pub fn shutdown() {
    if let Some(w) = WRITER.get().and_then(Option::as_ref) {
        w.shutdown();
    }
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
