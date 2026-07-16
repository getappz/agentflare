//! Compression retrieve registry (CCR) — makes optimize-layer compression
//! reversible: each compression registers its original under a short id;
//! `retrieve(id)` returns the original on demand.
//!
//! File-backed originals are snapshotted into a persistent, owned blob store
//! (`state_dir()/optimize/retrieve/blobs/`) at registration time, so a
//! registered original survives OS cache cleanup and mutation of the live
//! source. The index is written atomically (temp + rename) under a best-effort
//! advisory lock, so concurrent `agentflare` processes neither lose entries nor
//! observe a truncated index.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

pub const TTL_SECS: u64 = 7 * 24 * 60 * 60;
pub const MAX_ENTRIES: usize = 200;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "kind")]
pub enum EntryKind {
    FileBackup { backup_path: PathBuf },
    LeanCtxRead { handle: String },
    Inline { blob_path: PathBuf },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CompressionEntry {
    pub id: String,
    pub kind: EntryKind,
    pub size_before: u64,
    pub size_after: u64,
    pub created_ts: u64,
}

#[derive(Serialize, Deserialize, Default)]
pub struct RetrieveState {
    #[serde(default)]
    pub entries: HashMap<String, CompressionEntry>,
}

#[derive(Debug)]
pub enum RetrieveError {
    NotFound(String),
    Io(String),
    Delegated(String),
}

impl std::fmt::Display for RetrieveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RetrieveError::NotFound(id) => write!(
                f,
                "no compression registered under id '{id}' (expired or never existed)"
            ),
            RetrieveError::Io(e) => write!(f, "failed to read original: {e}"),
            RetrieveError::Delegated(cmd) => write!(f, "lean-ctx read — recover with: {cmd}"),
        }
    }
}

fn retrieve_dir() -> PathBuf {
    crate::state::state_dir().join("optimize").join("retrieve")
}

pub fn retrieve_state_path() -> PathBuf {
    retrieve_dir().join("index.json")
}

fn blobs_dir() -> PathBuf {
    retrieve_dir().join("blobs")
}

fn lock_path() -> PathBuf {
    retrieve_dir().join("index.lock")
}

pub fn load_state() -> RetrieveState {
    fs::read_to_string(retrieve_state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist the index atomically: write a per-process temp file, then rename it
/// over the index. `fs::rename` replaces the destination on both Unix and
/// Windows, so a reader never sees a half-written file and a crash mid-write
/// leaves the previous index intact rather than a truncated one.
pub fn save_state(state: &RetrieveState) {
    let path = retrieve_state_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        if fs::write(&tmp, json + "\n").is_ok() && fs::rename(&tmp, &path).is_err() {
            let _ = fs::remove_file(&tmp);
        }
    }
}

const LOCK_SPINS: u32 = 50;
const LOCK_SPIN_MS: u64 = 10;
const LOCK_STALE_SECS: u64 = 60;

fn lock_is_stale(path: &std::path::Path) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() >= LOCK_STALE_SECS)
}

/// Best-effort inter-process lock guarding the index read-modify-write.
///
/// Acquired by exclusively creating a lockfile; released (removed) on drop. If
/// the lock can't be taken within the spin budget it is *stolen* when stale
/// (holder likely crashed), otherwise we proceed unlocked — degrading to the
/// prior last-writer-wins behavior rather than blocking a compression forever.
struct LockGuard {
    path: PathBuf,
    held: bool,
}

impl LockGuard {
    fn acquire(path: PathBuf) -> Self {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        for _ in 0..LOCK_SPINS {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return LockGuard { path, held: true },
                Err(_) => {
                    if lock_is_stale(&path) {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(LOCK_SPIN_MS));
                }
            }
        }
        LockGuard { path, held: false }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if self.held {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Remove an entry, deleting its blob if we own one. Inline blobs live in our
/// blob store; FileBackup/LeanCtxRead point at files we don't own.
fn remove_entry(state: &mut RetrieveState, id: &str) {
    if let Some(EntryKind::Inline { blob_path }) = state.entries.remove(id).map(|e| e.kind) {
        let _ = fs::remove_file(blob_path);
    }
}

pub fn prune(state: &mut RetrieveState, now: u64) {
    let expired: Vec<String> = state
        .entries
        .iter()
        .filter(|(_, e)| now.saturating_sub(e.created_ts) >= TTL_SECS)
        .map(|(id, _)| id.clone())
        .collect();
    for id in expired {
        remove_entry(state, &id);
    }
    if state.entries.len() > MAX_ENTRIES {
        let mut by_age: Vec<(String, u64)> = state
            .entries
            .iter()
            .map(|(id, e)| (id.clone(), e.created_ts))
            .collect();
        by_age.sort_by_key(|(_, ts)| *ts);
        let excess = state.entries.len() - MAX_ENTRIES;
        for (id, _) in by_age.into_iter().take(excess) {
            remove_entry(state, &id);
        }
    }
}

/// Load the index, prune expired/over-cap entries (enforcing the TTL on read,
/// not just at registration), persist the pruned index if it changed, and
/// return it. Use this for enumerate/list paths so stale entries never surface.
pub fn active_state(now: u64) -> RetrieveState {
    let _lock = LockGuard::acquire(lock_path());
    let mut state = load_state();
    let before = state.entries.len();
    prune(&mut state, now);
    if state.entries.len() != before {
        save_state(&state);
    }
    state
}

fn origin_key(kind: &EntryKind) -> String {
    match kind {
        EntryKind::FileBackup { backup_path } => backup_path.to_string_lossy().to_string(),
        EntryKind::LeanCtxRead { handle } => handle.clone(),
        EntryKind::Inline { blob_path } => blob_path.to_string_lossy().to_string(),
    }
}

fn make_id(kind: &EntryKind, before: u64, after: u64) -> String {
    let mut h = DefaultHasher::new();
    origin_key(kind).hash(&mut h);
    before.hash(&mut h);
    after.hash(&mut h);
    format!("r-{:012x}", h.finish() & 0xffff_ffff_ffff)
}

pub fn register(kind: EntryKind, before: u64, after: u64, now: u64) -> CompressionEntry {
    // id is derived from the *incoming* origin (e.g. the FileBackup path) so it
    // stays deterministic and stable regardless of the blob snapshot below.
    let id = make_id(&kind, before, after);

    // Snapshot file-backed originals into a persistent, owned blob so the entry
    // survives OS cache cleanup and mutation of the live source (out-of-place
    // compression registers the live `source`). Best-effort: if the copy fails
    // we keep the original pointer kind rather than lose the entry entirely.
    let stored_kind = match &kind {
        EntryKind::FileBackup { backup_path } => {
            let dir = blobs_dir();
            let _ = fs::create_dir_all(&dir);
            let blob = dir.join(format!("{id}.orig"));
            match fs::copy(backup_path, &blob) {
                Ok(_) => EntryKind::Inline { blob_path: blob },
                Err(_) => kind.clone(),
            }
        }
        _ => kind.clone(),
    };

    let entry = CompressionEntry {
        id: id.clone(),
        kind: stored_kind,
        size_before: before,
        size_after: after,
        created_ts: now,
    };

    let _lock = LockGuard::acquire(lock_path());
    let mut state = load_state();
    state.entries.insert(id, entry.clone());
    prune(&mut state, now);
    save_state(&state);
    entry
}

pub fn retrieve(id: &str) -> Result<String, RetrieveError> {
    let state = load_state();
    let entry = state
        .entries
        .get(id)
        .ok_or_else(|| RetrieveError::NotFound(id.to_string()))?;
    match &entry.kind {
        EntryKind::FileBackup { backup_path } => {
            fs::read_to_string(backup_path).map_err(|e| RetrieveError::Io(e.to_string()))
        }
        EntryKind::Inline { blob_path } => {
            fs::read_to_string(blob_path).map_err(|e| RetrieveError::Io(e.to_string()))
        }
        EntryKind::LeanCtxRead { handle } => Err(RetrieveError::Delegated(format!(
            "ctx_read path=\"{handle}\" mode=raw"
        ))),
    }
}

fn human(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

pub fn marker(entry: &CompressionEntry) -> String {
    format!(
        "⟦optimize retrieve {} · {}→{} · expand: agentflare optimize retrieve {}⟧",
        entry.id,
        human(entry.size_before),
        human(entry.size_after),
        entry.id
    )
}

pub fn describe_origin(kind: &EntryKind) -> String {
    match kind {
        EntryKind::FileBackup { backup_path } => format!("file:{}", backup_path.display()),
        EntryKind::LeanCtxRead { handle } => format!("lean-ctx:{handle}"),
        EntryKind::Inline { blob_path } => format!("inline:{}", blob_path.display()),
    }
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;

    #[test]
    fn register_then_retrieve_roundtrips_file_backup() {
        with_temp_home(|| {
            let backup = crate::state::state_dir().join("orig.md");
            std::fs::create_dir_all(backup.parent().unwrap()).unwrap();
            std::fs::write(&backup, "ORIGINAL CONTENT").unwrap();
            let e = register(
                EntryKind::FileBackup {
                    backup_path: backup.clone(),
                },
                100,
                10,
                1_000,
            );
            assert_eq!(retrieve(&e.id).unwrap(), "ORIGINAL CONTENT");
        });
    }

    #[test]
    fn blob_snapshot_survives_source_deletion() {
        with_temp_home(|| {
            let src = crate::state::state_dir().join("src.md");
            std::fs::create_dir_all(src.parent().unwrap()).unwrap();
            std::fs::write(&src, "PERSISTED").unwrap();
            let e = register(
                EntryKind::FileBackup {
                    backup_path: src.clone(),
                },
                9,
                1,
                1,
            );
            // The live source may vanish (OS cache cleanup, or the user deletes it)...
            std::fs::remove_file(&src).unwrap();
            // ...yet retrieve still returns the snapshot from our owned blob store.
            assert_eq!(retrieve(&e.id).unwrap(), "PERSISTED");
            assert!(
                matches!(e.kind, EntryKind::Inline { .. }),
                "expected Inline snapshot, got {:?}",
                e.kind
            );
        });
    }

    #[test]
    fn snapshot_is_immutable_against_source_mutation() {
        with_temp_home(|| {
            let src = crate::state::state_dir().join("m.md");
            std::fs::create_dir_all(src.parent().unwrap()).unwrap();
            std::fs::write(&src, "ORIGINAL").unwrap();
            let e = register(
                EntryKind::FileBackup {
                    backup_path: src.clone(),
                },
                8,
                1,
                1,
            );
            std::fs::write(&src, "MUTATED").unwrap();
            assert_eq!(retrieve(&e.id).unwrap(), "ORIGINAL");
        });
    }

    #[test]
    fn id_is_deterministic_for_same_origin_and_sizes() {
        with_temp_home(|| {
            let k = || EntryKind::FileBackup {
                backup_path: "/x/y.md".into(),
            };
            assert_eq!(register(k(), 100, 10, 1).id, register(k(), 100, 10, 2).id);
        });
    }

    #[test]
    fn prune_drops_entries_past_ttl() {
        let mut s = RetrieveState::default();
        s.entries.insert(
            "r-old".into(),
            CompressionEntry {
                id: "r-old".into(),
                kind: EntryKind::FileBackup {
                    backup_path: "/a".into(),
                },
                size_before: 1,
                size_after: 1,
                created_ts: 0,
            },
        );
        prune(&mut s, TTL_SECS + 1);
        assert!(s.entries.is_empty());
    }

    #[test]
    fn prune_deletes_owned_blob_files() {
        with_temp_home(|| {
            let src = crate::state::state_dir().join("p.md");
            std::fs::create_dir_all(src.parent().unwrap()).unwrap();
            std::fs::write(&src, "BODY").unwrap();
            let e = register(EntryKind::FileBackup { backup_path: src }, 4, 1, 1);
            let blob = match &e.kind {
                EntryKind::Inline { blob_path } => blob_path.clone(),
                other => panic!("expected Inline, got {other:?}"),
            };
            assert!(blob.exists());
            let mut state = load_state();
            prune(&mut state, TTL_SECS + 2);
            assert!(
                !blob.exists(),
                "blob should be deleted when its entry is pruned"
            );
        });
    }

    #[test]
    fn active_state_omits_and_persists_prune_of_expired() {
        with_temp_home(|| {
            let src = crate::state::state_dir().join("a.md");
            std::fs::create_dir_all(src.parent().unwrap()).unwrap();
            std::fs::write(&src, "X").unwrap();
            register(EntryKind::FileBackup { backup_path: src }, 1, 1, 1);
            assert_eq!(active_state(1).entries.len(), 1); // fresh: kept
            assert!(active_state(TTL_SECS + 2).entries.is_empty()); // expired: pruned
            assert!(load_state().entries.is_empty()); // pruning was persisted
        });
    }

    #[test]
    fn save_state_leaves_no_temp_file() {
        with_temp_home(|| {
            let src = crate::state::state_dir().join("t.md");
            std::fs::create_dir_all(src.parent().unwrap()).unwrap();
            std::fs::write(&src, "Y").unwrap();
            register(EntryKind::FileBackup { backup_path: src }, 1, 1, 1);
            let dir = retrieve_state_path().parent().unwrap().to_path_buf();
            let leftover: Vec<String> = std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(|d| d.ok())
                .map(|d| d.file_name().to_string_lossy().into_owned())
                .filter(|n| n.contains(".tmp."))
                .collect();
            assert!(leftover.is_empty(), "temp files left: {leftover:?}");
        });
    }

    #[test]
    fn retrieve_unknown_id_errors() {
        with_temp_home(|| {
            assert!(matches!(
                retrieve("r-nope"),
                Err(RetrieveError::NotFound(_))
            ))
        });
    }

    #[test]
    fn lean_ctx_read_delegates_to_ctx_read_raw() {
        with_temp_home(|| {
            let e = register(
                EntryKind::LeanCtxRead {
                    handle: "src/x.rs".into(),
                },
                9,
                1,
                1,
            );
            match retrieve(&e.id) {
                Err(RetrieveError::Delegated(cmd)) => assert!(cmd.contains("mode=raw")),
                other => panic!("expected delegation, got {other:?}"),
            }
        });
    }

    #[test]
    fn id_is_48_bit_hex_width() {
        let k = EntryKind::FileBackup {
            backup_path: "/some/path.md".into(),
        };
        let id = make_id(&k, 1234, 56);
        assert!(id.starts_with("r-"), "id: {id}");
        assert_eq!(id.len(), 14, "expected r- + 12 hex chars: {id}");
        assert!(id[2..].chars().all(|c| c.is_ascii_hexdigit()), "id: {id}");
    }

    #[test]
    fn marker_contains_id_and_expand_command() {
        let e = CompressionEntry {
            id: "r-abc123".into(),
            kind: EntryKind::FileBackup {
                backup_path: "/a".into(),
            },
            size_before: 4200,
            size_after: 800,
            created_ts: 0,
        };
        let m = marker(&e);
        assert!(
            m.contains("r-abc123")
                && m.contains("agentflare optimize retrieve r-abc123")
                && m.contains("4.2k")
        );
    }
}
