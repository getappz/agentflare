use std::path::{Path, PathBuf};

use crate::model::{Identity, Lease};

/// JSON-file-backed lease store with atomic writes and corrupt-file
/// quarantine. All timestamps are seconds since the unix epoch and are passed
/// in by callers so the logic stays deterministic under test.
///
/// Every operation holds an internal mutex, so concurrent HTTP handlers and
/// the sweep loop within one process cannot interleave load-modify-save and
/// lose a heartbeat. Cross-process writers still race (last writer wins) but
/// cannot corrupt the file: writes go to a per-process temp file + rename.
pub struct LeaseStore {
    dir: PathBuf,
    guard: std::sync::Mutex<()>,
}

impl LeaseStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), guard: std::sync::Mutex::new(()) }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join("leases.json")
    }

    /// Load all leases. A missing file is an empty store. A corrupt file is
    /// moved aside to `leases.json.quarantine` and treated as empty — the
    /// supervisor must never crash-loop on bad state.
    pub fn load(&self) -> eyre::Result<Vec<Lease>> {
        let _guard = self.guard.lock().expect("lease store lock");
        self.load_unlocked()
    }

    fn load_unlocked(&self) -> eyre::Result<Vec<Lease>> {
        let path = self.path();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
        };
        match serde_json::from_str(&text) {
            Ok(leases) => Ok(leases),
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "corrupt lease file, quarantining");
                let quarantine = self.quarantine_path();
                let _ = std::fs::remove_file(&quarantine);
                std::fs::rename(&path, &quarantine)?;
                Ok(Vec::new())
            }
        }
    }

    /// Atomic save: write to a per-process temp file in the same directory,
    /// then rename over the target.
    pub fn save(&self, leases: &[Lease]) -> eyre::Result<()> {
        let _guard = self.guard.lock().expect("lease store lock");
        self.save_unlocked(leases)
    }

    fn save_unlocked(&self, leases: &[Lease]) -> eyre::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!("leases.json.tmp-{}", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec_pretty(leases)?)?;
        std::fs::rename(&tmp, self.path())?;
        Ok(())
    }

    pub fn create(
        &self,
        pid: u32,
        class: &str,
        ttl_seconds: u64,
        identity: Identity,
        allow_kill: bool,
        now: u64,
    ) -> eyre::Result<Lease> {
        let _guard = self.guard.lock().expect("lease store lock");
        let mut leases = self.load_unlocked()?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let lease = Lease {
            id: format!("l-{pid}-{now}-{nanos:x}"),
            pid,
            class: class.to_string(),
            created_at: now,
            ttl_seconds,
            identity,
            allow_kill,
        };
        leases.push(lease.clone());
        self.save_unlocked(&leases)?;
        Ok(lease)
    }

    /// Reset the lease clock. Returns the refreshed lease, or None when the
    /// id is unknown.
    pub fn heartbeat(&self, id: &str, now: u64) -> eyre::Result<Option<Lease>> {
        let _guard = self.guard.lock().expect("lease store lock");
        let mut leases = self.load_unlocked()?;
        let Some(lease) = leases.iter_mut().find(|l| l.id == id) else {
            return Ok(None);
        };
        lease.created_at = now;
        let refreshed = lease.clone();
        self.save_unlocked(&leases)?;
        Ok(Some(refreshed))
    }

    pub fn remove(&self, id: &str) -> eyre::Result<bool> {
        let _guard = self.guard.lock().expect("lease store lock");
        let mut leases = self.load_unlocked()?;
        let before = leases.len();
        leases.retain(|l| l.id != id);
        let removed = leases.len() < before;
        if removed {
            self.save_unlocked(&leases)?;
        }
        Ok(removed)
    }

    pub fn expired(&self, now: u64) -> eyre::Result<Vec<Lease>> {
        Ok(self.load()?.into_iter().filter(|l| l.expires_at() <= now).collect())
    }

    pub fn quarantine_path(&self) -> PathBuf {
        self.dir.join("leases.json.quarantine")
    }
}

pub fn default_state_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| Path::new(".").to_path_buf())
        .join("flared")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn store() -> (tempfile::TempDir, LeaseStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = LeaseStore::new(dir.path());
        (dir, store)
    }

    fn identity() -> Identity {
        Identity { exe_name: "agent.exe".into(), start_time: 500 }
    }

    #[test]
    fn create_then_load_roundtrips() {
        let (_dir, store) = store();
        let lease = store.create(4242, "build", 600, identity(), true, 1000).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded, vec![lease]);
    }

    #[test]
    fn load_on_missing_file_is_empty() {
        let (_dir, store) = store();
        assert_eq!(store.load().unwrap(), vec![]);
    }

    #[test]
    fn expired_returns_only_past_ttl() {
        let (_dir, store) = store();
        store.create(1, "a", 100, identity(), true, 1000).unwrap();
        let old = store.create(2, "b", 100, identity(), true, 200).unwrap();
        let expired = store.expired(1050).unwrap();
        assert_eq!(expired, vec![old]);
    }

    #[test]
    fn heartbeat_extends_expiry() {
        let (_dir, store) = store();
        let lease = store.create(1, "a", 100, identity(), true, 1000).unwrap();
        store.heartbeat(&lease.id, 1090).unwrap().unwrap();
        assert_eq!(store.expired(1150).unwrap(), vec![]);
        assert_eq!(store.expired(1191).unwrap().len(), 1);
    }

    #[test]
    fn heartbeat_unknown_id_is_none() {
        let (_dir, store) = store();
        assert!(store.heartbeat("nope", 1000).unwrap().is_none());
    }

    #[test]
    fn remove_deletes_lease() {
        let (_dir, store) = store();
        let lease = store.create(1, "a", 100, identity(), true, 1000).unwrap();
        assert!(store.remove(&lease.id).unwrap());
        assert_eq!(store.load().unwrap(), vec![]);
        assert!(!store.remove(&lease.id).unwrap());
    }

    #[test]
    fn corrupt_file_is_quarantined_not_fatal() {
        let (_dir, store) = store();
        std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        std::fs::write(store.path(), "{not json").unwrap();
        assert_eq!(store.load().unwrap(), vec![]);
        assert!(store.quarantine_path().exists(), "corrupt file should be moved aside");
        assert!(!store.path().exists());
    }
}
