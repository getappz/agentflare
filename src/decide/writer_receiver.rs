//! Receive independently of disk so hook exit can confirm handoff under slow I/O.
use super::*;
use std::sync::{Arc, mpsc};

enum DiskWork {
    Row(Vec<u8>),
    Flush(u64),
}

fn acknowledge(ack: &Mutex<Sender>, id: u64, receipt: bool, persisted: u64, pending: u64) {
    if let Ok(mut sender) = ack.try_lock()
        && let Ok(mut bytes) = serde_json::to_vec(&Ack {
            id,
            receipt,
            persisted_parent: persisted,
            pending_worker: pending,
        })
    {
        bytes.push(b'\n');
        sender.send(bytes);
    }
}

pub(super) fn run(address: &str, token: &str) -> io::Result<()> {
    let address: std::net::SocketAddr = address.parse().map_err(io::Error::other)?;
    if !address.ip().is_loopback() {
        return Err(io::Error::other("writer must be local"));
    }
    let token = hex::decode(token).map_err(io::Error::other)?;
    let connect = |role| -> io::Result<TcpStream> {
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
        stream.set_nodelay(true)?;
        stream.write_all(&[role])?;
        stream.write_all(&token)?;
        Ok(stream)
    };
    let data = connect(b'D')?;
    let mut control = connect(b'C')?;
    let ack = connect(b'A')?;
    ack.set_nonblocking(true)?;
    let ack = Arc::new(Mutex::new(Sender {
        stream: ack,
        pending: Vec::new(),
        offset: 0,
        broken: false,
    }));
    let mut reader = BufReader::new(data);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let init: Init = serde_json::from_str(&line)?;
    let parent = Arc::new(AtomicU64::new(0));
    let persisted = Arc::new(AtomicU64::new(0));
    let pending = Arc::new(AtomicU64::new(0));
    let count = parent.clone();
    let control_thread = std::thread::spawn(move || {
        let mut bytes = [0; 8];
        while control.read_exact(&mut bytes).is_ok() {
            count.fetch_max(u64::from_be_bytes(bytes), Ordering::Relaxed);
        }
    });
    #[cfg(not(test))]
    let capacity = 256;
    #[cfg(test)]
    let capacity = init.queue_capacity;
    let (tx, rx) = mpsc::sync_channel(capacity);
    let (worker_parent, worker_persisted, worker_pending, worker_ack) = (
        parent.clone(),
        persisted.clone(),
        pending.clone(),
        ack.clone(),
    );
    let ledger = init.ledger.clone();
    let disk = std::thread::spawn(move || {
        for work in rx {
            let result = match work {
                DiskWork::Row(frame) => {
                    #[cfg(test)]
                    std::thread::sleep(Duration::from_millis(init.delay_ms));
                    let (kind, value) = frame.split_at(1);
                    match kind {
                        b"D" => serde_json::from_slice::<shadow::Row>(value)
                            .map_err(io::Error::other)
                            .and_then(|row| storage::append(&init.ledger, &row, LEDGER_CAP)),
                        b"C" => serde_json::from_slice::<capture::Row>(value)
                            .map_err(io::Error::other)
                            .and_then(|row| storage::append(&init.dataset, &row, 10 * 1024 * 1024)),
                        _ => Err(io::Error::other("invalid writer message")),
                    }
                }
                DiskWork::Flush(id) => {
                    persist(
                        &init.ledger,
                        &worker_parent,
                        &worker_persisted,
                        &worker_pending,
                    );
                    acknowledge(
                        &worker_ack,
                        id,
                        false,
                        worker_persisted.load(Ordering::Relaxed),
                        worker_pending.load(Ordering::Relaxed),
                    );
                    continue;
                }
            };
            if result.is_err() {
                worker_pending.fetch_add(1, Ordering::Relaxed);
            }
            persist(
                &init.ledger,
                &worker_parent,
                &worker_persisted,
                &worker_pending,
            );
        }
        persist(
            &init.ledger,
            &worker_parent,
            &worker_persisted,
            &worker_pending,
        );
        #[cfg(test)]
        if init.delay_ms > 0 {
            let _ = storage::append(
                &init.ledger,
                &serde_json::json!({"record_type":"test_writer_closed"}),
                LEDGER_CAP,
            );
        }
    });
    acknowledge(&ack, u64::MAX, true, 0, 0);
    let mut frame = Vec::new();
    loop {
        frame.clear();
        let read = Read::by_ref(&mut reader)
            .take((MAX_FRAME + 1) as u64)
            .read_until(b'\n', &mut frame);
        let n = read.unwrap_or(frame.len());
        if n == 0 {
            break;
        }
        if !frame.ends_with(b"\n") || n > MAX_FRAME {
            if !matches!(frame.first(), Some(b'F' | b'H')) {
                pending.fetch_add(1, Ordering::Relaxed);
            }
            break;
        }
        let (kind, value) = frame.split_at(1);
        if kind == b"F" || kind == b"H" {
            if let Some((id, drops)) = std::str::from_utf8(value)
                .ok()
                .and_then(|s| s.trim().split_once(':'))
                && let (Ok(id), Ok(drops)) = (id.parse::<u64>(), drops.parse::<u64>())
            {
                parent.fetch_max(drops, Ordering::Relaxed);
                if kind == b"H" {
                    acknowledge(&ack, id, true, 0, 0);
                } else {
                    let _ = tx.try_send(DiskWork::Flush(id));
                }
            }
            continue;
        }
        if !matches!(kind, b"D" | b"C")
            || tx
                .try_send(DiskWork::Row(std::mem::take(&mut frame)))
                .is_err()
        {
            pending.fetch_add(1, Ordering::Relaxed);
        }
    }
    let _ = control_thread.join();
    drop(tx);
    let _ = disk.join();
    persist(&ledger, &parent, &persisted, &pending);
    Ok(())
}

fn persist(path: &std::path::Path, parent: &AtomicU64, persisted: &AtomicU64, pending: &AtomicU64) {
    let failed = pending.swap(0, Ordering::Relaxed);
    let total = parent.load(Ordering::Relaxed);
    let count = failed + total.saturating_sub(persisted.load(Ordering::Relaxed));
    if count == 0 {
        return;
    }
    if storage::append(
        path,
        &serde_json::json!({"record_type":"dropped","count":count}),
        LEDGER_CAP,
    )
    .is_ok()
    {
        persisted.store(total, Ordering::Relaxed);
    } else {
        pending.fetch_add(failed, Ordering::Relaxed);
    }
}
