//! Shared, best-effort circuit breaker for short-lived hook processes.
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FAILURES: u32 = 3;
const WINDOW: u64 = 60;
const COOLDOWN: u64 = 60;

#[derive(Default, Serialize, Deserialize)]
struct State {
    failures: u32,
    last_failure: u64,
    open_until: u64,
    probe_until: u64,
}

pub fn path() -> PathBuf {
    crate::paths::agentflare_dir()
        .join("decide")
        .join("breaker.json")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn with_state<T>(path: &Path, f: impl FnOnce(&mut State) -> (T, bool)) -> std::io::Result<T> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut file = opts.open(path)?;
    file.lock_exclusive()?;
    let mut raw = Vec::new();
    file.read_to_end(&mut raw)?;
    let mut state = serde_json::from_slice::<State>(&raw).unwrap_or_default();
    let (result, changed) = f(&mut state);
    if changed {
        let bytes = serde_json::to_vec(&state).map_err(std::io::Error::other)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&bytes)?;
        file.set_len(bytes.len() as u64)?;
    }
    Ok(result)
}

/// Reserve the only recovery probe. State I/O failure leaves the call enabled.
pub fn allow(path: &Path, now: u64, timeout: Duration) -> bool {
    with_state(path, |s| {
        if s.probe_until > now || s.open_until > now {
            return (false, false);
        }
        if s.open_until != 0 {
            s.probe_until = now.saturating_add(timeout.as_secs().max(1) + 5);
            return (true, true);
        }
        (true, false)
    })
    .unwrap_or(true)
}

pub fn observe(path: &Path, now: u64, success: bool) {
    let _ = with_state(path, |s| {
        if success {
            *s = State::default();
        } else if s.probe_until != 0 {
            s.probe_until = 0;
            s.open_until = now.saturating_add(COOLDOWN);
        } else {
            s.failures = if now.saturating_sub(s.last_failure) <= WINDOW {
                s.failures.saturating_add(1)
            } else {
                1
            };
            s.last_failure = now;
            if s.failures >= FAILURES {
                s.open_until = now.saturating_add(COOLDOWN);
            }
        }
        ((), true)
    });
}

pub fn status() -> String {
    let time = now();
    with_state(&path(), |s| {
        let label = if s.probe_until > time {
            "probe in progress".to_string()
        } else if s.open_until > time {
            format!("open ({}s remaining)", s.open_until - time)
        } else if s.open_until != 0 {
            "ready for probe".to_string()
        } else {
            format!("closed ({} consecutive failures)", s.failures)
        };
        (label, false)
    })
    .unwrap_or_else(|_| "unavailable (state file inaccessible)".to_string())
}

pub fn current_time() -> u64 {
    now()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trips_probes_and_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("breaker.json");
        for n in 0..3 {
            assert!(allow(&path, 100 + n, Duration::from_secs(1)));
            observe(&path, 100 + n, false);
        }
        assert!(!allow(&path, 120, Duration::from_secs(1)));
        assert!(allow(&path, 162, Duration::from_secs(1)));
        assert!(!allow(&path, 162, Duration::from_secs(1)));
        observe(&path, 163, true);
        assert!(allow(&path, 163, Duration::from_secs(1)));
    }

    #[test]
    fn missing_corrupt_and_concurrent_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("breaker.json");
        assert!(allow(&path, 100, Duration::from_secs(1)));
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert!(allow(&path, 100, Duration::from_secs(1)));
        std::thread::scope(|scope| {
            for _ in 0..16 {
                let path = &path;
                scope.spawn(move || observe(path, 100, false));
            }
        });
        let raw = std::fs::read_to_string(&path).unwrap();
        let state: State = serde_json::from_str(&raw).unwrap();
        assert_eq!(state.failures, 16);
        assert!(!allow(&path, 101, Duration::from_secs(1)));
    }

    #[test]
    fn failures_outside_window_reset_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("breaker.json");
        observe(&path, 100, false);
        observe(&path, 161, false);
        let state: State = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(state.failures, 1);
    }

    #[test]
    fn ordinary_success_resets_failures_and_failed_probe_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("breaker.json");
        observe(&path, 100, false);
        observe(&path, 101, true);
        observe(&path, 102, false);
        observe(&path, 103, false);
        assert!(allow(&path, 103, Duration::from_secs(1)));
        observe(&path, 104, false);
        assert!(!allow(&path, 105, Duration::from_secs(1)));
        assert!(allow(&path, 164, Duration::from_secs(1)));
        observe(&path, 165, false);
        assert!(!allow(&path, 166, Duration::from_secs(1)));
    }
}
