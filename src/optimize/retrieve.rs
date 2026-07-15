//! Compression retrieve registry (CCR) — makes optimize-layer compression
//! reversible: each compression registers its original under a short id;
//! `retrieve(id)` returns the original on demand.

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

pub fn retrieve_state_path() -> PathBuf {
    crate::state::state_dir()
        .join("optimize")
        .join("retrieve")
        .join("index.json")
}

pub fn load_state() -> RetrieveState {
    fs::read_to_string(retrieve_state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_state(state: &RetrieveState) {
    if let Some(parent) = retrieve_state_path().parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = fs::write(retrieve_state_path(), json + "\n");
    }
}

pub fn prune(state: &mut RetrieveState, now: u64) {
    state
        .entries
        .retain(|_, e| now.saturating_sub(e.created_ts) < TTL_SECS);
    if state.entries.len() > MAX_ENTRIES {
        let mut by_age: Vec<(String, u64)> = state
            .entries
            .iter()
            .map(|(id, e)| (id.clone(), e.created_ts))
            .collect();
        by_age.sort_by_key(|(_, ts)| *ts);
        let excess = state.entries.len() - MAX_ENTRIES;
        for (id, _) in by_age.into_iter().take(excess) {
            state.entries.remove(&id);
        }
    }
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
    format!("r-{:06x}", h.finish() & 0xff_ffff)
}

pub fn register(kind: EntryKind, before: u64, after: u64, now: u64) -> CompressionEntry {
    let id = make_id(&kind, before, after);
    let entry = CompressionEntry {
        id: id.clone(),
        kind,
        size_before: before,
        size_after: after,
        created_ts: now,
    };
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
