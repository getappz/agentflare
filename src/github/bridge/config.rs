//! Bridge configuration. Environment-driven for v1, matching current
//! practice; item #331 (unified `~/.agentflare/config.toml`) should absorb
//! these later.

use std::io::Write;
use std::path::{Path, PathBuf};

pub const CLAIMED_LABEL_PREFIX: &str = "claimed:";

const DEFAULT_INTERVAL_SECS: u64 = 60;
/// Floor so a mistyped interval cannot turn the loop into a hot GitHub poll.
pub const MIN_INTERVAL_SECS: u64 = 15;
const DEFAULT_MAX_CLAIMS: usize = 3;
const DEFAULT_QUEUE_LABEL: &str = "agentflare";

#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub enabled: bool,
    pub interval_secs: u64,
    /// Max issues this instance will claim per tick. `0` is LEGITIMATE and is
    /// NOT floored: it is deliberate "drain mode" — stop claiming new work
    /// while this instance keeps re-verifying and exporting issues it
    /// already holds. Useful for taking an instance out of rotation without
    /// dropping its in-flight work.
    pub max_claims: usize,
    pub ttl_secs: i64,
    pub queue_label: String,
    pub instance_id: String,
}

fn truthy(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes")
}

/// `~/.agentflare/bridge-instance-id` — the persisted discriminator half of
/// this workstation's bridge owner id.
fn instance_id_path() -> PathBuf {
    crate::paths::home()
        .join(".agentflare")
        .join("bridge-instance-id")
}

/// Owner-id prefix, in place of the `<agent>` half `claims::owner_id()` uses.
///
/// Deliberately a CONSTANT rather than the detected agent name. The agent is
/// a property of whatever launched the daemon, not of the daemon: started
/// from a Claude Code session it detects `claude-code`, started by launchd or
/// systemd it detects nothing and falls back to `cli`. Same machine, same
/// persisted file, two different owner ids — and since marker liveness is
/// judged per owner id, the second one cedes everything the first was
/// holding. That is the C2 mass-cede all over again, just triggered by a
/// change of launcher instead of a change of pid.
///
/// The actor here is the bridge daemon. Naming it that is both stable and
/// more accurate than naming whichever agent happened to start it.
const OWNER_PREFIX: &str = "bridge";

fn random_instance() -> String {
    use rand::Rng;
    let bytes: [u8; 6] = rand::thread_rng().r#gen();
    format!("{OWNER_PREFIX}:{}", hex::encode(bytes))
}

/// How long a loser of the create race waits for the winner to finish
/// writing. Generously long for a few bytes to a local file; the only way to
/// exhaust it is a process that died between creating the file and filling
/// it, which is a real (if rare) state and reported rather than papered over.
const INSTANCE_ID_WRITE_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

fn stored_instance(path: &Path) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    // A file written before the prefix moved into the stored value holds a
    // bare discriminator. Prefix it rather than re-minting, so an existing
    // workstation keeps an id that is at least stable from here on.
    Some(if t.contains(':') {
        t.to_string()
    } else {
        format!("{OWNER_PREFIX}:{t}")
    })
}

/// Reads the persisted instance discriminator, minting and storing one on
/// first use. Kept pure over `path` so tests need no home override.
///
/// `create_new` rather than a plain write: two agentflare processes starting
/// at once must converge on ONE id, so the loser of the create race reads the
/// winner's value instead of overwriting it with its own.
///
/// The loser then has to WAIT. `create_new` publishes the file the instant it
/// is created, which is before the winner has written anything into it, so a
/// loser that reads immediately sees an empty file and concludes the id is
/// unusable. That window is microseconds wide and was duly caught by the
/// concurrency test rather than in production, where the fallback would have
/// been a pid-derived id and a scary warning on a perfectly healthy machine.
fn read_or_create_instance(path: &Path) -> std::io::Result<String> {
    if let Some(existing) = stored_instance(path) {
        return Ok(existing);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let fresh = random_instance();
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut f) => {
            f.write_all(fresh.as_bytes())?;
            f.sync_all()?;
            Ok(fresh)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let deadline = std::time::Instant::now() + INSTANCE_ID_WRITE_WAIT;
            loop {
                if let Some(existing) = stored_instance(path) {
                    return Ok(existing);
                }
                if std::time::Instant::now() >= deadline {
                    return Err(std::io::Error::other(format!(
                        "{} exists but is still empty after {INSTANCE_ID_WRITE_WAIT:?} — \
                         a process probably died mid-write; delete it to re-mint",
                        path.display()
                    )));
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        Err(e) => Err(e),
    }
}

/// `bridge:<persisted-id>` — the bridge's owner identity.
///
/// Read WHOLE from disk. Nothing about it is derived from the environment at
/// startup, which is the point: marker liveness is judged per owner id, so
/// any part of the id that can change between runs mass-cedes everything the
/// previous run was holding.
///
/// Deliberately NOT `claims::owner_id()`, which fails that test twice over —
/// its instance half is `AGENTFLARE_SESSION` or the **pid**, and its agent
/// half is whatever agent happens to be detected (see [`OWNER_PREFIX`]).
///
/// The persisted id identifies the WORKSTATION, not the process. Running two
/// bridge daemons against one home directory would make them a single owner
/// in each other's eyes; set `AGENTFLARE_BRIDGE_INSTANCE_ID` explicitly to
/// keep them distinct. It survives restarts, and survives reinstalling the
/// binary — it lives under `~/.agentflare/`, not next to the executable. It
/// does NOT survive deleting that directory or moving to a new machine,
/// which is correct: that is a different workstation.
pub fn stable_instance_id() -> String {
    match read_or_create_instance(&instance_id_path()) {
        Ok(id) => id,
        Err(e) => {
            // Falling back to the pid keeps this tick working but makes the
            // NEXT restart cede everything — say so rather than fail silently.
            eprintln!(
                "github bridge: cannot persist an instance id ({e}); falling back to a \
                 process-scoped one. Every restart will cede its in-flight work until \
                 this is fixed or AGENTFLARE_BRIDGE_INSTANCE_ID is set."
            );
            crate::claims::owner_id()
        }
    }
}

impl BridgeConfig {
    pub fn from_env() -> BridgeConfig {
        let get = |k: &str| std::env::var(k).ok();
        let instance = get("AGENTFLARE_BRIDGE_INSTANCE_ID")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(stable_instance_id);
        BridgeConfig::from_values(
            get("AGENTFLARE_BRIDGE_ENABLED").as_deref(),
            get("AGENTFLARE_BRIDGE_INTERVAL_SECS").as_deref(),
            get("AGENTFLARE_BRIDGE_MAX_CLAIMS").as_deref(),
            get("AGENTFLARE_BRIDGE_QUEUE_LABEL").as_deref(),
            instance,
        )
    }

    /// Split out from `from_env` so the parsing rules are testable without
    /// mutating process-global environment state.
    pub fn from_values(
        enabled: Option<&str>,
        interval: Option<&str>,
        max_claims: Option<&str>,
        queue_label: Option<&str>,
        instance_id: String,
    ) -> BridgeConfig {
        BridgeConfig {
            enabled: enabled.is_some_and(truthy),
            interval_secs: interval
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(DEFAULT_INTERVAL_SECS)
                .max(MIN_INTERVAL_SECS),
            max_claims: max_claims
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(DEFAULT_MAX_CLAIMS),
            // Reuses the EXISTING claim TTL so marker liveness and the local
            // ledger expire on one schedule.
            ttl_secs: crate::claims::ttl_secs(),
            queue_label: queue_label
                .filter(|s| !s.is_empty())
                .unwrap_or(DEFAULT_QUEUE_LABEL)
                .to_string(),
            instance_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_off_and_conservative() {
        let c = BridgeConfig::from_values(None, None, None, None, "agent:1".to_string());
        assert!(!c.enabled, "bridge must be opt-in");
        assert_eq!(c.interval_secs, 60);
        assert_eq!(c.max_claims, 3);
        assert_eq!(c.queue_label, "agentflare");
        assert_eq!(c.instance_id, "agent:1");
    }

    #[test]
    fn values_parse_from_strings() {
        let c = BridgeConfig::from_values(
            Some("1"),
            Some("15"),
            Some("7"),
            Some("queue"),
            "agent:1".to_string(),
        );
        assert!(c.enabled);
        assert_eq!(c.interval_secs, 15);
        assert_eq!(c.max_claims, 7);
        assert_eq!(c.queue_label, "queue");
    }

    #[test]
    fn enabled_accepts_common_truthy_spellings() {
        for v in ["1", "true", "TRUE", "yes"] {
            let c = BridgeConfig::from_values(Some(v), None, None, None, "a".to_string());
            assert!(c.enabled, "{v} should enable");
        }
        for v in ["0", "false", "no", "", "banana"] {
            let c = BridgeConfig::from_values(Some(v), None, None, None, "a".to_string());
            assert!(!c.enabled, "{v} should not enable");
        }
    }

    #[test]
    fn garbage_numbers_fall_back_to_defaults_rather_than_panicking() {
        let c = BridgeConfig::from_values(
            Some("1"),
            Some("not-a-number"),
            Some(""),
            None,
            "a".to_string(),
        );
        assert_eq!(c.interval_secs, 60);
        assert_eq!(c.max_claims, 3);
    }

    #[test]
    fn interval_has_a_floor_so_a_typo_cannot_hammer_github() {
        let c = BridgeConfig::from_values(Some("1"), Some("0"), None, None, "a".to_string());
        assert_eq!(c.interval_secs, MIN_INTERVAL_SECS);
    }

    #[test]
    fn a_persisted_instance_id_survives_a_restart() {
        // The whole point of C2: the id must NOT change between processes.
        // `read_or_create_instance` called twice models exactly that — a
        // pid- or session-derived id would differ on the second call.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("bridge-instance-id");

        let first = read_or_create_instance(&path).unwrap();
        assert!(!first.is_empty());
        assert_eq!(
            read_or_create_instance(&path).unwrap(),
            first,
            "a restart must reuse the persisted id, not mint a new one"
        );
    }

    #[test]
    fn two_processes_racing_to_create_converge_on_one_id() {
        // Repeated, because the window this guards is microseconds wide:
        // `create_new` publishes the file before the winner has written a
        // byte into it, so a loser that reads immediately sees it empty. A
        // single round hits that maybe one time in twenty.
        for round in 0..25 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("bridge-instance-id");

            let ids: Vec<String> = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..8)
                    .map(|_| {
                        scope.spawn(|| {
                            read_or_create_instance(&path).unwrap_or_else(|e| {
                                panic!("round {round}: a racing creator failed: {e}")
                            })
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });

            assert!(
                ids.windows(2).all(|w| w[0] == w[1]),
                "round {round}: concurrent creators must all end up with the \
                 file's single id, got {ids:?}"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), ids[0]);
        }
    }

    #[test]
    fn a_file_left_empty_by_a_dead_writer_is_reported_not_silently_used() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bridge-instance-id");
        // An empty instance half would render as `agent:` — a marker owner
        // that silently collides with every other blank-file workstation.
        std::fs::write(&path, "   \n").unwrap();
        // The file exists, so `create_new` fails. Nobody is going to fill it,
        // so after the write-wait this must surface as an error naming the
        // file, not an empty owner id.
        let err = read_or_create_instance(&path).expect_err("must not yield an empty id");
        assert!(
            err.to_string().contains("bridge-instance-id"),
            "the error has to name the file to delete: {err}"
        );
    }

    #[test]
    fn a_stored_id_is_trimmed_and_a_legacy_bare_one_gets_the_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bridge-instance-id");
        std::fs::write(&path, "  bridge:abc123\n").unwrap();
        assert_eq!(read_or_create_instance(&path).unwrap(), "bridge:abc123");

        // Written before the prefix moved into the file: adopt it rather than
        // re-mint, so the workstation keeps one id from here on.
        let legacy = dir.path().join("legacy");
        std::fs::write(&legacy, "abc123").unwrap();
        assert_eq!(read_or_create_instance(&legacy).unwrap(), "bridge:abc123");
    }

    #[test]
    fn the_whole_owner_id_comes_from_disk_not_from_the_environment() {
        // The bug this pins, found by running two daemons on one machine:
        // the id used to be `<detected-agent>:<persisted>`. Started from a
        // Claude Code session the agent detects as `claude-code`; started by
        // launchd or systemd it detects nothing and falls back to `cli`. Same
        // machine, same file, two owner ids — so the second run ceded
        // everything the first was holding. Exactly the C2 mass-cede, just
        // triggered by a change of launcher rather than a change of pid.
        //
        // Asserted structurally rather than by mutating process-global env:
        // nothing in the returned id may come from anywhere but the file.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bridge-instance-id");
        let id = read_or_create_instance(&path).unwrap();

        assert_eq!(
            id,
            std::fs::read_to_string(&path).unwrap().trim(),
            "the id must be exactly what is on disk, with nothing spliced in"
        );
        assert!(
            id.starts_with(&format!("{OWNER_PREFIX}:")),
            "and must carry the constant prefix, not a detected agent name: {id}"
        );
        // The detected agent must not appear anywhere in it.
        let detected = crate::claims::agent_of(&crate::claims::owner_id()).to_string();
        assert!(
            detected.is_empty() || !id.contains(&detected) || detected == OWNER_PREFIX,
            "the detected agent {detected:?} leaked into the owner id {id:?} — \
             it would change the moment the daemon is started by something else"
        );
    }

    #[test]
    fn max_claims_zero_is_legal_drain_mode_not_a_floor_violation() {
        let c = BridgeConfig::from_values(Some("1"), None, Some("0"), None, "a".to_string());
        assert_eq!(
            c.max_claims, 0,
            "0 must pass through unfloored: it means drain mode (stop claiming \
             new work, keep re-verifying/exporting what's already held), not \
             a mistyped value to correct"
        );
    }
}
