//! Bounded background writes; shutdown accounts for work left after its deadline.
use super::{
    capture,
    storage::{self, LEDGER_CAP, Pending, Work},
};
use std::{
    collections::BTreeMap,
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

enum Message {
    Row(u64, Work),
    Flush(mpsc::SyncSender<()>),
    Stop,
}

#[derive(Default)]
struct Book {
    next: u64,
    pending: BTreeMap<u64, Pending>,
    stopped: bool,
}

pub(super) struct Writer {
    tx: mpsc::SyncSender<Message>,
    book: Arc<Mutex<Book>>,
    dropped: Arc<AtomicU64>,
    ledger: PathBuf,
    dataset: PathBuf,
    closing: AtomicBool,
    worker: Mutex<Option<JoinHandle<()>>>,
    recovery: Mutex<Vec<std::process::Child>>,
}

impl Writer {
    pub fn new(ledger: PathBuf, dataset: PathBuf) -> io::Result<Self> {
        Self::start(ledger, dataset, 256, Duration::ZERO)
    }

    #[cfg(test)]
    pub fn delayed(ledger: PathBuf, dataset: PathBuf, delay_ms: u64) -> io::Result<Self> {
        Self::start(ledger, dataset, 256, Duration::from_millis(delay_ms))
    }

    #[cfg(test)]
    pub fn constrained(ledger: PathBuf, dataset: PathBuf, delay_ms: u64) -> io::Result<Self> {
        Self::start(ledger, dataset, 1, Duration::from_millis(delay_ms))
    }

    fn start(
        ledger: PathBuf,
        dataset: PathBuf,
        capacity: usize,
        _delay: Duration,
    ) -> io::Result<Self> {
        if std::env::var_os(super::recovery::ACTIVE).is_some() {
            return Err(io::Error::other("recovery cannot start a writer"));
        }
        #[cfg(test)]
        if std::env::var("AGENTFLARE_TEST_LEDGER_START_FAILURE").as_deref() == Ok("1") {
            return Err(io::Error::other("injected thread startup failure"));
        }
        let (tx, rx) = mpsc::sync_channel(capacity);
        let book = Arc::new(Mutex::new(Book::default()));
        let dropped = Arc::new(AtomicU64::new(0));
        let (worker_book, worker_drops, path, worker_dataset) = (
            book.clone(),
            dropped.clone(),
            ledger.clone(),
            dataset.clone(),
        );
        let worker = std::thread::Builder::new()
            .name("decide-ledger".into())
            .spawn(move || {
                while let Ok(message) = rx.recv() {
                    if worker_book
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .stopped
                    {
                        break;
                    }
                    match message {
                        Message::Row(ticket, work) => {
                            let mut batch = vec![(ticket, work)];
                            let mut barrier = None;
                            while batch.len() < 256 {
                                let Ok(next) = rx.try_recv() else {
                                    break;
                                };
                                match next {
                                    Message::Row(ticket, work) => batch.push((ticket, work)),
                                    Message::Flush(ack) => {
                                        barrier = Some(ack);
                                        break;
                                    }
                                    Message::Stop => break,
                                }
                            }
                            #[cfg(test)]
                            std::thread::sleep(_delay);
                            let complete = |ticket: u64, success: bool| {
                                let mut book = worker_book
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                if !book.pending.contains_key(&ticket) {
                                    return;
                                }
                                // After the shutdown snapshot, its identified drop record owns failures.
                                if !success && !book.stopped {
                                    worker_drops.fetch_add(1, Ordering::Relaxed);
                                }
                                if success || !book.stopped {
                                    book.pending.remove(&ticket);
                                }
                            };
                            let allowed = |_: u64| {
                                !worker_book
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .stopped
                            };
                            let decisions = batch.iter().filter_map(|(ticket, work)| match work {
                                Work::Decision(row) => Some((*ticket, row.as_ref())),
                                _ => None,
                            });
                            if capture::append_many(
                                &path,
                                decisions,
                                LEDGER_CAP,
                                &allowed,
                                |ticket| complete(ticket, true),
                                false,
                            )
                            .is_err()
                            {
                                for (ticket, work) in &batch {
                                    if matches!(work, Work::Decision(_)) {
                                        complete(*ticket, false);
                                    }
                                }
                            }
                            let captures = batch.iter().filter_map(|(ticket, work)| match work {
                                Work::Capture(row) => Some((*ticket, row.as_ref())),
                                _ => None,
                            });
                            if capture::append_many(
                                &worker_dataset,
                                captures,
                                10 * 1024 * 1024,
                                &allowed,
                                |ticket| complete(ticket, true),
                                false,
                            )
                            .is_err()
                            {
                                for (ticket, work) in &batch {
                                    if matches!(work, Work::Capture(_)) {
                                        complete(*ticket, false);
                                    }
                                }
                            }
                            if let Some(ack) = barrier {
                                let _ = ack.send(());
                            }
                        }
                        Message::Flush(ack) => {
                            let _ = ack.send(());
                        }
                        Message::Stop => break,
                    }
                }
            })?;
        Ok(Self {
            tx,
            book,
            dropped,
            ledger,
            dataset,
            closing: AtomicBool::new(false),
            worker: Mutex::new(Some(worker)),
            recovery: Mutex::new(Vec::new()),
        })
    }

    pub fn enqueue(&self, mut work: Work) {
        let mut pending = work.pending();
        let serialized = match &work {
            Work::Decision(row) => serde_json::to_vec(row),
            Work::Capture(row) => serde_json::to_vec(row),
        };
        let bytes = match serialized {
            Ok(bytes) if bytes.len() < 64 * 1024 && !self.closing.load(Ordering::Acquire) => bytes,
            _ => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };
        pending.bytes = Arc::new(bytes);
        // This lock protects queue bookkeeping only; disk I/O never holds it.
        let mut book = self
            .book
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if book.stopped || self.closing.load(Ordering::Acquire) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let ticket = book.next;
        book.next += 1;
        book.pending.insert(ticket, pending);
        if self.tx.try_send(Message::Row(ticket, work)).is_err() {
            book.pending.remove(&ticket);
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn pending_drops(&self) -> u64 {
        let book = self
            .book
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.dropped.load(Ordering::Relaxed)
            + if book.stopped {
                book.pending.len() as u64
            } else {
                0
            }
    }

    pub fn flush(&self, timeout: Duration) -> bool {
        let flushed = self.barrier(timeout);
        let _ = storage::persist_drops(&self.ledger, &self.dropped, &[]);
        flushed
    }

    fn barrier(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let (tx, rx) = mpsc::sync_channel(1);
        let mut message = Message::Flush(tx);
        let sent = loop {
            match self.tx.try_send(message) {
                Ok(()) => break true,
                Err(mpsc::TrySendError::Full(returned)) if Instant::now() < deadline => {
                    message = returned;
                    std::thread::sleep(Duration::from_micros(100));
                }
                _ => break false,
            }
        };
        sent && rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_ok()
    }

    fn handoff_pending(&self) {
        let pending: Vec<_> = self
            .book
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .iter()
            .map(|(ticket, row)| (*ticket, row.clone()))
            .collect();
        let rows: Vec<_> = pending.iter().map(|(_, row)| row.clone()).collect();
        if let Ok(child) =
            super::recovery::handoff(&self.ledger, &self.dataset, &self.dropped, &rows)
        {
            if let Some(child) = child {
                self.recovery
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(child);
            }
            let mut book = self
                .book
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for (ticket, _) in pending {
                book.pending.remove(&ticket);
            }
        }
    }

    /// Producers must finish before shutdown; no disk writer is joined on hook exit.
    pub fn shutdown(&self) {
        if !self.closing.swap(true, Ordering::AcqRel) {
            let _ = self.barrier(Duration::from_millis(50));
        }
        self.book
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stopped = true;
        let _ = self.tx.try_send(Message::Stop);
        self.handoff_pending();
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.shutdown();
        #[cfg(test)]
        if let Ok(worker) = self.worker.get_mut()
            && let Some(worker) = worker.take()
        {
            let _ = worker.join();
            self.handoff_pending();
        }
        #[cfg(test)]
        if let Ok(children) = self.recovery.get_mut() {
            for mut child in children.drain(..) {
                let _ = child.wait();
            }
        }
    }
}
