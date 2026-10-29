//! A local helper owns disk I/O, so ordinary caller exit cannot kill queued writes.
use super::{
    capture, shadow,
    storage::{self, LEDGER_CAP, Work},
};
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

const MAX_FRAME: usize = 70 * 1024;
const ENTRYPOINT: &str = "--decide-writer-internal";

#[derive(Serialize, Deserialize)]
struct Init {
    ledger: PathBuf,
    dataset: PathBuf,
    #[cfg(test)]
    delay_ms: u64,
    #[cfg(test)]
    queue_capacity: usize,
}

#[derive(Serialize, Deserialize)]
struct Ack {
    id: u64,
    receipt: bool,
    persisted_parent: u64,
    pending_worker: u64,
}

struct Sender {
    stream: TcpStream,
    pending: Vec<u8>,
    offset: usize,
    broken: bool,
}

impl Sender {
    fn drain(&mut self) {
        while self.offset < self.pending.len() && !self.broken {
            match self.stream.write(&self.pending[self.offset..]) {
                Ok(0) => self.broken = true,
                Ok(n) => self.offset += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.broken = true;
                    let _ = self.stream.shutdown(Shutdown::Write);
                }
            }
        }
        if self.offset == self.pending.len() {
            self.pending.clear();
            self.offset = 0;
        }
    }

    fn send(&mut self, bytes: Vec<u8>) -> bool {
        self.drain();
        if self.broken || !self.pending.is_empty() {
            return false;
        }
        self.pending = bytes;
        self.drain();
        if !self.pending.is_empty() && self.offset == 0 {
            self.pending.clear();
            return false;
        }
        true
    }
}

pub(super) struct Writer {
    data: Mutex<Sender>,
    control: Mutex<Sender>,
    ack: Mutex<BufReader<TcpStream>>,
    dropped: AtomicU64,
    persisted: AtomicU64,
    worker_pending: AtomicU64,
    sequence: AtomicU64,
    child: Mutex<Option<Child>>,
    closed: AtomicBool,
}

impl Writer {
    pub fn new(ledger: PathBuf, dataset: PathBuf) -> io::Result<Self> {
        Self::start(Init {
            ledger,
            dataset,
            #[cfg(test)]
            delay_ms: 0,
            #[cfg(test)]
            queue_capacity: 256,
        })
    }

    #[cfg(test)]
    pub fn delayed(ledger: PathBuf, dataset: PathBuf, delay_ms: u64) -> io::Result<Self> {
        Self::start(Init {
            ledger,
            dataset,
            delay_ms,
            queue_capacity: 256,
        })
    }

    #[cfg(test)]
    pub fn constrained(ledger: PathBuf, dataset: PathBuf, delay_ms: u64) -> io::Result<Self> {
        Self::start(Init {
            ledger,
            dataset,
            delay_ms,
            queue_capacity: 1,
        })
    }

    fn start(init: Init) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?.to_string();
        let token = rand::random::<[u8; 16]>();
        let mut command = Command::new(std::env::current_exe()?);
        #[cfg(not(test))]
        command.args([ENTRYPOINT, &address, &hex::encode(token)]);
        #[cfg(test)]
        command
            .args([
                "--exact",
                "decide::storage::tests::writer_child",
                "--ignored",
            ])
            .env(
                "AGENTFLARE_TEST_WRITER",
                format!("{address} {}", hex::encode(token)),
            );
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(3);
        let connected = (|| {
            let (mut data, control, ack) = accept_all(&listener, &token, deadline)?;
            serde_json::to_writer(&mut data, &init)?;
            data.write_all(b"\n")?;
            data.set_nonblocking(true)?;
            control.set_nonblocking(true)?;
            ack.set_read_timeout(Some(
                deadline
                    .saturating_duration_since(Instant::now())
                    .max(Duration::from_millis(1)),
            ))?;
            let mut ack = BufReader::new(ack);
            let mut ready = String::new();
            ack.read_line(&mut ready)?;
            let ready: Ack = serde_json::from_str(&ready)?;
            if ready.id != u64::MAX || !ready.receipt {
                return Err(io::Error::other("invalid writer readiness acknowledgement"));
            }
            ack.get_ref().set_read_timeout(None)?;
            ack.get_ref().set_nonblocking(true)?;
            let sender = |stream| {
                Mutex::new(Sender {
                    stream,
                    pending: Vec::new(),
                    offset: 0,
                    broken: false,
                })
            };
            Ok(Self {
                data: sender(data),
                control: sender(control),
                ack: Mutex::new(ack),
                dropped: AtomicU64::new(0),
                persisted: AtomicU64::new(0),
                worker_pending: AtomicU64::new(0),
                sequence: AtomicU64::new(0),
                child: Mutex::new(None),
                closed: AtomicBool::new(false),
            })
        })();
        match connected {
            Ok(w) => {
                *w.child
                    .lock()
                    .map_err(|_| io::Error::other("writer lock poisoned"))? = Some(child);
                Ok(w)
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(e)
            }
        }
    }

    fn frame(&self, bytes: Vec<u8>) -> bool {
        bytes.len() <= MAX_FRAME && self.data.try_lock().is_ok_and(|mut s| s.send(bytes))
    }

    fn notify_drops(&self) {
        if let Ok(mut control) = self.control.try_lock() {
            control.send(self.dropped.load(Ordering::Relaxed).to_be_bytes().to_vec());
        }
    }

    pub fn enqueue(&self, work: Work) {
        let (kind, serialized) = match work {
            Work::Decision(row) => (b'D', serde_json::to_vec(&row)),
            Work::Capture(row) => (b'C', serde_json::to_vec(&row)),
        };
        let sent = serialized.is_ok_and(|mut bytes| {
            bytes.insert(0, kind);
            bytes.push(b'\n');
            self.frame(bytes)
        });
        if !sent {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            self.notify_drops();
        }
    }

    pub fn pending_drops(&self) -> u64 {
        self.dropped
            .load(Ordering::Relaxed)
            .saturating_sub(self.persisted.load(Ordering::Relaxed))
            + self.worker_pending.load(Ordering::Relaxed)
    }

    pub fn flush(&self, timeout: Duration) -> bool {
        self.barrier(timeout, false)
    }

    pub fn receipt(&self, timeout: Duration) -> bool {
        self.barrier(timeout, true)
    }

    fn barrier(&self, timeout: Duration, receipt: bool) -> bool {
        self.notify_drops();
        let id = self.sequence.fetch_add(1, Ordering::Relaxed);
        let Ok(mut reader) = self.ack.try_lock() else {
            return false;
        };
        let deadline = Instant::now() + timeout;
        let kind = if receipt { 'H' } else { 'F' };
        let frame = format!("{kind}{id}:{}\n", self.dropped.load(Ordering::Relaxed)).into_bytes();
        while !self.frame(frame.clone()) {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::yield_now();
        }
        let mut line = String::new();
        loop {
            match reader.read_line(&mut line) {
                Ok(0) => return false,
                Ok(_) if line.ends_with('\n') => {
                    if let Ok(ack) = serde_json::from_str::<Ack>(&line) {
                        if !ack.receipt {
                            self.persisted
                                .store(ack.persisted_parent, Ordering::Relaxed);
                            self.worker_pending
                                .store(ack.pending_worker, Ordering::Relaxed);
                        }
                        if ack.id == id && ack.receipt == receipt {
                            return true;
                        }
                    }
                    line.clear();
                }
                Err(e)
                    if e.kind() != io::ErrorKind::WouldBlock
                        && e.kind() != io::ErrorKind::Interrupted =>
                {
                    return false;
                }
                _ => {}
            }
            if Instant::now() >= deadline {
                return false;
            }
            if receipt {
                std::thread::yield_now();
            } else {
                std::thread::sleep(Duration::from_micros(100));
            }
        }
    }

    pub fn shutdown(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        // Confirm receipt in the helper's memory, never wait for its disk writer.
        self.receipt(Duration::from_millis(20));
        self.notify_drops();
        for mutex in [&self.data, &self.control] {
            if let Ok(mut s) = mutex.try_lock() {
                s.drain();
                let _ = s.stream.shutdown(Shutdown::Write);
            }
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.shutdown();
        #[cfg(test)]
        if let Ok(child) = self.child.get_mut()
            && let Some(mut child) = child.take()
        {
            let _ = child.wait();
        }
    }
}

fn accept(
    listener: &TcpListener,
    token: &[u8; 16],
    deadline: Instant,
) -> io::Result<(u8, TcpStream)> {
    loop {
        match listener.accept() {
            Ok((mut s, _)) => {
                // Windows accepts can inherit the listener's nonblocking mode.
                s.set_nonblocking(false)?;
                s.set_read_timeout(Some(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .max(Duration::from_millis(1)),
                ))?;
                let mut hello = [0; 17];
                if s.read_exact(&mut hello).is_ok() && hello[1..] == *token {
                    s.set_nodelay(true)?;
                    s.set_read_timeout(None)?;
                    return Ok((hello[0], s));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(1))
            }
            Err(e) => return Err(e),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "writer startup timed out",
            ));
        }
    }
}

fn accept_all(
    listener: &TcpListener,
    token: &[u8; 16],
    deadline: Instant,
) -> io::Result<(TcpStream, TcpStream, TcpStream)> {
    let (mut data, mut control, mut ack) = (None, None, None);
    while data.is_none() || control.is_none() || ack.is_none() {
        let (role, stream) = accept(listener, token, deadline)?;
        match role {
            b'D' if data.is_none() => data = Some(stream),
            b'C' if control.is_none() => control = Some(stream),
            b'A' if ack.is_none() => ack = Some(stream),
            _ => return Err(io::Error::other("invalid or duplicate writer channel")),
        }
    }
    Ok((data.unwrap(), control.unwrap(), ack.unwrap()))
}

#[cfg(test)]
#[path = "writer_protocol_tests.rs"]
mod tests;

pub fn worker_entrypoint() -> bool {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_none_or(|a| a != ENTRYPOINT) {
        return false;
    }
    if let (Some(address), Some(token)) = (args.get(2), args.get(3)) {
        let _ = run(address, token);
    }
    true
}

#[path = "writer_receiver.rs"]
mod receiver;

pub(super) fn run(address: &str, token: &str) -> io::Result<()> {
    receiver::run(address, token)
}
