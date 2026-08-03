//! Bridge configuration. Environment-driven for v1, matching current
//! practice; item #331 (unified `~/.agentflare/config.toml`) should absorb
//! these later.

use std::io::Write;
use std::path::{Path, PathBuf};

#[allow(dead_code)]
pub const CLAIMED_LABEL_PREFIX: &str = "claimed:";

const DEFAULT_INTERVAL_SECS: u64 = 60;
/// Floor so a mistyped interval cannot turn the loop into a hot GitHub poll.
pub const MIN_INTERVAL_SECS: u64 = 15;
const DEFAULT_MAX_CLAIMS: usize = 3;
const DEFAULT_QUEUE_LABEL: &str = "agentflare";

#[derive(Debug, Clone)]
#[allow(dead_code)]
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

fn random_instance() -> String {
    use rand::Rng;
    let bytes: [u8; 6] = rand::thread_rng().r#gen();
    hex::encode(bytes)
}

/// Reads the persisted instance discriminator, minting and storing one on
/// first use. Kept pure over `path` so tests need no home override.
///
/// `create_new` rather than a plain write: two agentflare processes starting
/// at once must converge on ONE id, so the loser of the create race reads the
/// winner's value instead of overwriting it with its own.
fn read_or_create_instance(path: &Path) -> std::io::Result<String> {
    let stored = |p: &Path| -> Option<String> {
        let s = std::fs::read_to_string(p).ok()?;
        let t = s.trim();
        (!t.is_empty()).then(|| t.to_string())
    };
    if let Some(existing) = stored(path) {
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
            Ok(fresh)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => stored(path).ok_or_else(|| {
            std::io::Error::other(format!(
                "{} exists but is empty; delete it to re-mint",
                path.display()
            ))
        }),
        Err(e) => Err(e),
    }
}

/// `<agent>:<persisted-id>` — the bridge's owner identity.
///
/// Deliberately NOT `claims::owner_id()`, whose instance half is
/// `AGENTFLARE_SESSION` or the **pid**. Marker liveness is judged per owner
/// id, so a pid-derived id means every daemon restart is a brand-new owner:
/// `i_hold` goes false against the markers the previous process wrote, and
/// the bridge cedes and cancels every in-flight item it was actually still
/// working. The discriminator therefore has to outlive the process, so it is
/// persisted on disk.
///
/// The persisted half identifies the WORKSTATION, not the process. Running
/// two bridge daemons against one home directory would make them a single
/// owner in each other's eyes; set `AGENTFLARE_BRIDGE_INSTANCE_ID`
/// explicitly to keep them distinct.
pub fn stable_instance_id() -> String {
    let agent = crate::claims::agent_of(&crate::claims::owner_id()).to_string();
    match read_or_create_instance(&instance_id_path()) {
        Ok(instance) => format!("{agent}:{instance}"),
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

#[allow(dead_code)]
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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bridge-instance-id");

        let ids: Vec<String> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| read_or_create_instance(&path).unwrap()))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        assert!(
            ids.windows(2).all(|w| w[0] == w[1]),
            "concurrent creators must all end up with the file's single id, got {ids:?}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), ids[0]);
    }

    #[test]
    fn a_blank_or_whitespace_file_is_re_minted_rather_than_used() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bridge-instance-id");
        // An empty instance half would render as `agent:` — a marker owner
        // that silently collides with every other blank-file workstation.
        std::fs::write(&path, "   \n").unwrap();
        // The file exists, so `create_new` fails; with nothing readable in it
        // this must surface as an error, not an empty owner id.
        assert!(read_or_create_instance(&path).is_err());
    }

    #[test]
    fn a_stored_id_is_trimmed_not_taken_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bridge-instance-id");
        std::fs::write(&path, "  abc123\n").unwrap();
        assert_eq!(read_or_create_instance(&path).unwrap(), "abc123");
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
