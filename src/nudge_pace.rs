//! Shared cooldown-based pacing for nudge text (coaching-rule bodies, the
//! vent-judge hook's block reason) so the same key doesn't refire on every
//! matching call. One flat JSON map, global across concurrent sessions —
//! same semantics as vent's own THROTTLE_WINDOW_SECS throttle
//! (src/vent/capture.rs), just keyed by a stable id instead of vent's fuzzy
//! free-text topic_key.
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

pub const DEFAULT_COOLDOWN: Duration = Duration::from_secs(300);

/// Comfortably larger than any `cooldown_secs` a coaching rule is expected to
/// configure, so `mark_fired`'s opportunistic pruning never erases a key
/// before its own (possibly custom, unbounded) cooldown has elapsed.
const MAX_RETENTION_SECS: i64 = 30 * 24 * 60 * 60;

fn state_path() -> PathBuf {
    crate::state::state_dir().join("nudge-pace.json")
}

fn lock_path() -> PathBuf {
    crate::state::state_dir().join(".nudge-pace.lock")
}

/// A cross-process advisory lock over the load-mutate-save sequence in
/// `mark_fired`, same pattern as coaching's `RulesLock`
/// (src/coaching/store.rs): `create_new` on a sentinel file fails if another
/// process already holds it. Without this, two hook processes racing on
/// `mark_fired` — even for different keys — can each `load()` before the
/// other's `save()` lands, silently dropping one process's fired-timestamp.
struct PaceLock {
    path: PathBuf,
}

impl PaceLock {
    fn acquire() -> std::io::Result<Self> {
        let path = lock_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut last_err = None;
        for attempt in 0..400u32 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Self { path }),
                Err(e) => {
                    // A holder that crashed without cleaning up (or, on
                    // Windows, a transient AV/indexer handle on the lock
                    // file) must never wedge nudge pacing shut: every ~2s
                    // of contention, clear the lock and keep retrying —
                    // rather than stealing it once and propagating any
                    // further failure, which silently drops the caller's
                    // write instead of just waiting longer.
                    if attempt > 0 && attempt % 200 == 0 {
                        let _ = std::fs::remove_file(&path);
                    }
                    last_err = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
        Err(last_err.unwrap_or_else(|| std::io::Error::other("could not acquire nudge-pace lock")))
    }
}

impl Drop for PaceLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn load() -> HashMap<String, String> {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(map: &HashMap<String, String>) {
    let path = state_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(map) {
        // Atomic replace: this file is written from short-lived hook
        // processes that run under a few seconds' timeout and can be killed
        // mid-write, and parallel tool calls invoke them concurrently. A pid
        // suffix keeps two concurrent writers from sharing one tmp path.
        let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
        if std::fs::write(&tmp, json).is_ok() && std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// True if `key` has never fired, or its last recorded fire is older than
/// `cooldown`. Fails open (returns true) on any read/parse error.
pub fn should_fire(key: &str, cooldown: Duration) -> bool {
    let map = load();
    let Some(last) = map.get(key) else {
        return true;
    };
    let Ok(last) = chrono::DateTime::parse_from_rfc3339(last) else {
        return true;
    };
    let last = last.with_timezone(&chrono::Utc);
    let now = chrono::Utc::now();
    now < last || (now - last).num_seconds() as u64 >= cooldown.as_secs()
}

/// Records `key` as having fired now. Best-effort: write failures (including
/// a failure to acquire `PaceLock`) are swallowed, never surfaced as an error
/// to the caller. Opportunistically prunes entries older than
/// `MAX_RETENTION_SECS` so the file never grows past the number of recently
/// distinct keys.
pub fn mark_fired(key: &str) {
    let Ok(_lock) = PaceLock::acquire() else {
        return;
    };
    let mut map = load();
    let now = chrono::Utc::now();
    map.retain(|_, ts| {
        chrono::DateTime::parse_from_rfc3339(ts)
            .map(|t| (now - t.with_timezone(&chrono::Utc)).num_seconds() < MAX_RETENTION_SECS)
            .unwrap_or(false)
    });
    map.insert(key.to_string(), now.to_rfc3339());
    save(&map);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;

    #[test]
    fn should_fire_is_true_for_a_never_seen_key() {
        with_temp_home(|| {
            assert!(should_fire("coaching:hygiene", Duration::from_secs(300)));
        });
    }

    #[test]
    fn should_fire_is_false_immediately_after_mark_fired() {
        with_temp_home(|| {
            mark_fired("coaching:hygiene");
            assert!(!should_fire("coaching:hygiene", Duration::from_secs(300)));
        });
    }

    #[test]
    fn should_fire_is_true_again_once_a_stale_timestamp_is_past_cooldown() {
        with_temp_home(|| {
            let path = state_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut map = std::collections::HashMap::new();
            map.insert(
                "coaching:hygiene".to_string(),
                (chrono::Utc::now() - chrono::Duration::seconds(400)).to_rfc3339(),
            );
            std::fs::write(&path, serde_json::to_string(&map).unwrap()).unwrap();

            assert!(should_fire("coaching:hygiene", Duration::from_secs(300)));
        });
    }

    #[test]
    fn should_fire_fails_open_on_a_corrupt_state_file() {
        with_temp_home(|| {
            let path = state_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "not json").unwrap();
            assert!(should_fire("coaching:hygiene", Duration::from_secs(300)));
        });
    }

    #[test]
    fn mark_fired_is_independent_per_key() {
        with_temp_home(|| {
            mark_fired("coaching:hygiene");
            assert!(should_fire("coaching:other-rule", Duration::from_secs(300)));
            assert!(!should_fire("coaching:hygiene", Duration::from_secs(300)));
        });
    }

    #[test]
    fn mark_fired_survives_concurrent_writers_for_different_keys() {
        // Without PaceLock serializing the load-mutate-save sequence, two
        // processes racing on mark_fired can each load() before the other's
        // save() lands, silently dropping one's fired-timestamp.
        with_temp_home(|| {
            std::thread::scope(|scope| {
                for i in 0..8 {
                    scope.spawn(move || {
                        mark_fired(&format!("key-{i}"));
                    });
                }
            });
            for i in 0..8 {
                assert!(
                    !should_fire(&format!("key-{i}"), Duration::from_secs(300)),
                    "key-{i}'s fired timestamp was lost to a concurrent writer"
                );
            }
        });
    }

    #[test]
    fn mark_fired_does_not_prune_an_entry_younger_than_max_retention() {
        with_temp_home(|| {
            let path = state_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut map = HashMap::new();
            // Older than the previous hardcoded 24h prune window, but well
            // within a rule's allowed cooldown_secs (unbounded u64).
            map.insert(
                "coaching:long-cooldown".to_string(),
                (chrono::Utc::now() - chrono::Duration::hours(25)).to_rfc3339(),
            );
            std::fs::write(&path, serde_json::to_string(&map).unwrap()).unwrap();

            mark_fired("coaching:other-rule");

            assert!(
                load().contains_key("coaching:long-cooldown"),
                "an entry younger than MAX_RETENTION_SECS must survive an unrelated mark_fired call"
            );
        });
    }
}
