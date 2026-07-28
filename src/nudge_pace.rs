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

fn state_path() -> PathBuf {
    crate::state::state_dir().join("nudge-pace.json")
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
        let _ = std::fs::write(path, json);
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

/// Records `key` as having fired now. Best-effort: write failures are
/// swallowed, never surfaced as an error to the caller. Opportunistically
/// prunes entries older than 24h so the file never grows past the number of
/// recently distinct keys.
pub fn mark_fired(key: &str) {
    let mut map = load();
    let now = chrono::Utc::now();
    map.retain(|_, ts| {
        chrono::DateTime::parse_from_rfc3339(ts)
            .map(|t| (now - t.with_timezone(&chrono::Utc)).num_seconds() < 86_400)
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
}
