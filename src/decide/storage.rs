//! Bounded best-effort writer: hooks only try_send, never wait for disk or a lock.
use super::{capture, shadow};
use serde::{Serialize, de::DeserializeOwned};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::time::Duration;

pub const LEDGER_CAP: u64 = 5 * 1024 * 1024;
static WRITER: OnceLock<Option<Writer>> = OnceLock::new();
static START_FAILURES: AtomicU64 = AtomicU64::new(0);

enum Work {
    Decision(Box<shadow::Row>),
    Capture(Box<capture::Row>),
    Flush(mpsc::SyncSender<()>),
}

struct Writer {
    tx: mpsc::SyncSender<Work>,
    dropped: Arc<AtomicU64>,
}

impl Writer {
    fn new(ledger: PathBuf, dataset: PathBuf) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel(256);
        let dropped = Arc::new(AtomicU64::new(0));
        let pending = dropped.clone();
        std::thread::Builder::new()
            .name("decide-ledger".into())
            .spawn(move || {
                for work in rx {
                    let result = match work {
                        Work::Decision(row) => append(&ledger, &*row, LEDGER_CAP),
                        Work::Capture(row) => append(&dataset, &*row, 10 * 1024 * 1024),
                        Work::Flush(ack) => {
                            persist_drops(&ledger, &pending);
                            let _ = ack.try_send(());
                            continue;
                        }
                    };
                    if result.is_err() {
                        pending.fetch_add(1, Ordering::Relaxed);
                    }
                    persist_drops(&ledger, &pending);
                }
                persist_drops(&ledger, &pending);
            })?;
        Ok(Self { tx, dropped })
    }

    fn enqueue(&self, work: Work) {
        if self.tx.try_send(work).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn flush(&self, timeout: Duration) -> bool {
        let (ack, rx) = mpsc::sync_channel(1);
        self.tx.try_send(Work::Flush(ack)).is_ok() && rx.recv_timeout(timeout).is_ok()
    }
}

fn writer() -> Option<&'static Writer> {
    WRITER
        .get_or_init(|| Writer::new(shadow::log_path(), capture::dataset_path()).ok())
        .as_ref()
}

fn persist_drops(path: &Path, dropped: &AtomicU64) {
    let count = dropped.swap(0, Ordering::Relaxed);
    if count > 0
        && append(
            path,
            &serde_json::json!({"record_type":"dropped", "count":count}),
            LEDGER_CAP,
        )
        .is_err()
    {
        dropped.fetch_add(count, Ordering::Relaxed);
    }
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
            .map_or(0, |w| w.dropped.load(Ordering::Relaxed))
}

/// No worker is created with all flags off. Hooks get at most 1 ms at shutdown;
/// callers outside hooks may explicitly flush longer before reporting/exiting.
pub fn flush(timeout: Duration) -> bool {
    WRITER
        .get()
        .and_then(Option::as_ref)
        .is_none_or(|w| w.flush(timeout))
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
