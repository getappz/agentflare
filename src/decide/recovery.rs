//! One-shot recovery after a deadline; never waits for child startup or disk in the caller.
use super::{
    capture,
    storage::{self, LEDGER_CAP, Pending},
};
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const ENTRYPOINT: &str = "--decide-recover-internal";
const PAYLOAD: &str = "AGENTFLARE_DECIDE_RECOVERY_PAYLOAD";
pub(super) const ACTIVE: &str = "AGENTFLARE_DECIDE_RECOVERY";
const MAX_PAYLOAD: usize = 30 * 1024; // Windows environment values are limited to 32,767 characters.

#[derive(Serialize, Deserialize)]
struct Row {
    capture: bool,
    json: String,
}

#[derive(Serialize, Deserialize)]
struct Packet {
    ledger: PathBuf,
    dataset: PathBuf,
    dropped: serde_json::Value,
    rows: Vec<Row>,
}

pub(super) fn handoff(
    ledger: &Path,
    dataset: &Path,
    counter: &AtomicU64,
    pending: &[Pending],
) -> io::Result<Option<Child>> {
    let count = counter.swap(0, Ordering::AcqRel);
    if count == 0 && pending.is_empty() {
        return Ok(None);
    }
    let result = (|| {
        if std::env::var_os(ACTIVE).is_some() {
            return Err(io::Error::other("recovery cannot spawn recovery"));
        }
        let mut packet = Packet {
            ledger: ledger.into(),
            dataset: dataset.into(),
            dropped: storage::drop_record(count, pending),
            rows: Vec::new(),
        };
        let mut used = serde_json::to_vec(&packet)?.len();
        if used > MAX_PAYLOAD {
            return Err(io::Error::other("recovery metadata exceeds limit"));
        }
        for row in pending {
            let budget = row.bytes.len().saturating_mul(2) + 64;
            if used + budget <= MAX_PAYLOAD
                && let Ok(json) = std::str::from_utf8(&row.bytes)
            {
                packet.rows.push(Row {
                    capture: row.capture,
                    json: json.into(),
                });
                used += budget;
            }
        }
        let payload = serde_json::to_string(&packet)?;
        let mut command = Command::new(std::env::current_exe()?);
        #[cfg(not(test))]
        command.arg(ENTRYPOINT);
        #[cfg(test)]
        command.args([
            "--exact",
            "decide::storage::lifecycle_tests::recovery_child",
            "--ignored",
        ]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        command
            .env(ACTIVE, "1")
            .env(PAYLOAD, payload)
            .env_remove("AGENTFLARE_TEST_LEDGER_LOCK_DELAY")
            .env_remove("AGENTFLARE_TEST_LEDGER_START_FAILURE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(Some)
    })();
    if result.is_err() {
        counter.fetch_add(count, Ordering::Relaxed);
    }
    result
}

pub fn entrypoint() -> Option<i32> {
    if std::env::args().nth(1).as_deref() != Some(ENTRYPOINT) {
        return None;
    }
    Some(if run_from_env().is_ok() { 0 } else { 1 })
}

pub(super) fn run_from_env() -> io::Result<()> {
    if std::env::var(ACTIVE).as_deref() != Ok("1") {
        return Err(io::Error::other("missing recovery marker"));
    }
    let payload = std::env::var(PAYLOAD).map_err(io::Error::other)?;
    if payload.len() > MAX_PAYLOAD {
        return Err(io::Error::other("recovery payload exceeds limit"));
    }
    let packet: Packet = serde_json::from_str(&payload)?;
    let rows: Vec<_> = packet
        .rows
        .iter()
        .map(|r| serde_json::from_str::<serde_json::Value>(&r.json).map(|v| (r.capture, v)))
        .collect::<Result<_, _>>()?;
    let _watchdog = std::thread::Builder::new()
        .name("decide-recovery-deadline".into())
        .spawn(|| {
            std::thread::sleep(Duration::from_secs(5));
            std::process::exit(1);
        })?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let attempt = {
            for (is_capture, path, cap) in [
                (false, &packet.ledger, LEDGER_CAP),
                (true, &packet.dataset, 10 * 1024 * 1024),
            ] {
                // Failed restoration must still leave its identified loss in the ledger.
                let _ = capture::append_many(
                    path,
                    rows.iter()
                        .enumerate()
                        .filter(|(_, (capture, _))| *capture == is_capture)
                        .map(|(i, (_, row))| (i as u64, row)),
                    cap,
                    |_| true,
                    |_| {},
                    true,
                );
            }
            storage::append(&packet.ledger, &packet.dropped, LEDGER_CAP)
        };
        if attempt.is_ok() || Instant::now() >= deadline {
            return attempt;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
