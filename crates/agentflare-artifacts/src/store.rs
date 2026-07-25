use crate::types::{
    Artifact, ArtifactSummary, ArtifactType, GitProvenance, PublishRequest, PublishResponse,
    VersionInfo,
};
use agentflare_store::{documents::DocUpsertOpts, Store};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const ARTIFACTS_DIR: &str = "artifacts";
const META_FILE: &str = "meta.json";
const CONTENT_FILE: &str = "content";

const VERSIONS_DIR: &str = "versions";

/// Version snapshots beyond this many most-recent are pruned on publish — a
/// bound on runaway republish loops (e.g. a stuck `/loop` session hammering
/// one artifact), not a limit normal editing ever reaches. `diff`/
/// `get_version` on a pruned version returns NotFound; `versions()` (the
/// history list) is untouched, so what happened is still visible even after
/// old snapshot bodies are gone. v1 is always kept as the origin anchor.
const MAX_KEPT_VERSIONS: u32 = 50;

/// Doc-store project key for artifact documents.
const DOC_PROJECT: &str = "__artifacts__";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ArtifactMeta {
    pub id: String,
    pub name: String,
    pub artifact_type: ArtifactType,
    pub session_id: String,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favicon: Option<String>,
    #[serde(default)]
    pub history: Vec<VersionInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitProvenance>,
}

#[derive(Clone)]
pub struct ArtifactStore {
    base_path: PathBuf,
    live_broadcast: Arc<Mutex<HashMap<String, Vec<std::sync::mpsc::Sender<String>>>>>,
    store: Option<Arc<Store>>,
}

impl ArtifactStore {
    pub fn new(base_path: PathBuf) -> Self {
        let store = ArtifactStore {
            base_path: base_path.join(ARTIFACTS_DIR),
            live_broadcast: Arc::new(Mutex::new(HashMap::new())),
            store: None,
        };
        let _ = fs::create_dir_all(&store.base_path);
        store
    }

    pub fn with_store(store: Store) -> Self {
        ArtifactStore {
            base_path: PathBuf::from("store:"),
            live_broadcast: Arc::new(Mutex::new(HashMap::new())),
            store: Some(Arc::new(store)),
        }
    }

    /// True when the store is backed by agentflare_store documents+blobs.
    pub fn has_store(&self) -> bool {
        self.store.is_some()
    }

    fn store_conn_err(e: impl std::fmt::Display) -> std::io::Error {
        std::io::Error::other(e.to_string())
    }

    fn doc_to_artifact(doc: &agentflare_store::documents::Document) -> std::io::Result<Artifact> {
        let meta: ArtifactMeta = serde_json::from_str(&doc.metadata)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(Artifact {
            id: doc.path.clone(),
            name: doc.title.clone(),
            artifact_type: meta.artifact_type,
            content: doc.content.clone(),
            session_id: doc.session_id.clone().unwrap_or_default(),
            created_at: doc.created_at as u64,
            updated_at: doc.updated_at as u64,
            version: doc.version as u32,
            description: meta.description,
            favicon: meta.favicon,
            sender: meta.sender,
            recipient: meta.recipient,
            thread_id: meta.thread_id,
            reply_to: meta.reply_to,
            git: meta.git,
        })
    }

    fn doc_to_summary(doc: &agentflare_store::documents::Document) -> ArtifactSummary {
        let meta: Option<ArtifactMeta> = serde_json::from_str(&doc.metadata).ok();
        let art_type = meta
            .as_ref()
            .map(|m| m.artifact_type.clone())
            .or_else(|| doc.tags.first().map(|s| ArtifactType::from(s.as_str())))
            .unwrap_or(ArtifactType::Text);
        ArtifactSummary {
            id: doc.path.clone(),
            name: doc.title.clone(),
            artifact_type: art_type,
            session_id: doc.session_id.clone().unwrap_or_default(),
            created_at: doc.created_at as u64,
            updated_at: doc.updated_at as u64,
            version: doc.version as u32,
            description: meta.as_ref().and_then(|m| m.description.clone()),
            favicon: meta.as_ref().and_then(|m| m.favicon.clone()),
            sender: meta.as_ref().and_then(|m| m.sender.clone()),
            recipient: meta.as_ref().and_then(|m| m.recipient.clone()),
            thread_id: meta.as_ref().and_then(|m| m.thread_id.clone()),
            reply_to: meta.as_ref().and_then(|m| m.reply_to.clone()),
        }
    }

    fn meta_to_metadata(
        prev: Option<&ArtifactMeta>,
        req: &PublishRequest,
        new_version: u32,
        now: i64,
    ) -> String {
        let keep = |new: &Option<String>, old: fn(&ArtifactMeta) -> Option<String>| {
            new.clone().or_else(|| prev.and_then(old))
        };
        let mut history = prev.map(|m| m.history.clone()).unwrap_or_default();
        history.push(VersionInfo {
            version: new_version,
            label: req.label.clone(),
            created_at: now as u64,
        });
        let meta = ArtifactMeta {
            id: String::new(),
            name: req.name.clone(),
            artifact_type: req.artifact_type.clone(),
            session_id: req.session_id.clone(),
            created_at: prev.map(|m| m.created_at).unwrap_or(now as u64),
            updated_at: now as u64,
            version: new_version,
            description: keep(&req.description, |m| m.description.clone()),
            favicon: keep(&req.favicon, |m| m.favicon.clone()),
            history,
            sender: keep(&req.sender, |m| m.sender.clone()),
            recipient: keep(&req.recipient, |m| m.recipient.clone()),
            thread_id: keep(&req.thread_id, |m| m.thread_id.clone()),
            reply_to: keep(&req.reply_to, |m| m.reply_to.clone()),
            git: req.git.clone().or_else(|| prev.and_then(|m| m.git.clone())),
        };
        serde_json::to_string(&meta).unwrap_or_else(|_| "{}".into())
    }

    pub fn publish(&self, req: &PublishRequest) -> std::io::Result<PublishResponse> {
        if let Some(ref store) = self.store {
            return self.publish_store(store, req);
        }
        self.publish_flat(req)
    }

    fn publish_store(
        &self,
        store: &Store,
        req: &PublishRequest,
    ) -> std::io::Result<PublishResponse> {
        let id = req.update_id.clone().unwrap_or_else(|| nanoid::nanoid!());
        let now = db_kit::ids::now();
        let path = &id;

        // Read existing doc to check CAS and dedup
        let existing = store
            .doc_get_by_path(DOC_PROJECT, path)
            .map_err(Self::store_conn_err)?;

        if let (Some(base), Some(doc)) = (req.base_version, existing.as_ref()) {
            if base as i32 != doc.version {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "version conflict: base_version {base}, but current version is {}",
                        doc.version
                    ),
                ));
            }
        }

        let unchanged = existing.as_ref().is_some_and(|doc| {
            if !doc.content.is_empty() {
                doc.content == req.content
            } else {
                let new_hash = blake3::hash(req.content.as_bytes()).to_hex().to_string();
                doc.blob_hash.as_deref() == Some(&new_hash)
            }
        });

        if !unchanged {
            // Store content as blob for size efficiency
            let blob_hash = store
                .blob_store(req.content.as_bytes())
                .map_err(Self::store_conn_err)?;

            let prev_meta: Option<ArtifactMeta> = existing
                .as_ref()
                .and_then(|doc| serde_json::from_str(&doc.metadata).ok());

            let next_version = existing.as_ref().map(|d| d.version as u32 + 1).unwrap_or(1);
            let metadata = Self::meta_to_metadata(prev_meta.as_ref(), req, next_version, now);

            store
                .doc_upsert_with_opts(
                    DOC_PROJECT,
                    path,
                    "",
                    DocUpsertOpts {
                        title: Some(req.name.clone()),
                        doc_type: Some("artifact".into()),
                        blob_hash: Some(blob_hash),
                        mime: Some(req.artifact_type.mime_type().into()),
                        tags: Some(vec![req.artifact_type.to_string()]),
                        session_id: Some(req.session_id.clone()),
                        source: Some("artifact".into()),
                        metadata: Some(metadata),
                        size: Some(req.content.len() as i64),
                        ..Default::default()
                    },
                )
                .map_err(Self::store_conn_err)?;
        }

        let doc = store
            .doc_get_by_path(DOC_PROJECT, path)
            .map_err(Self::store_conn_err)?
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "artifact not found after upsert",
                )
            })?;

        let version = doc.version as u32;
        let response = PublishResponse {
            id: id.clone(),
            url: format!("/{id}"),
            session_id: req.session_id.clone(),
            version,
        };

        if existing.is_some() && !unchanged {
            self.broadcast(&id, "refresh");
        }

        Ok(response)
    }

    fn publish_flat(&self, req: &PublishRequest) -> std::io::Result<PublishResponse> {
        let id = req
            .update_id
            .clone()
            .filter(|uid| self.artifact_dir(uid).exists())
            .unwrap_or_else(|| nanoid::nanoid!());

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let prev = self.read_meta(&id);

        if let (Some(base), Some(prev_meta)) = (req.base_version, prev.as_ref()) {
            if base != prev_meta.version {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "version conflict: base_version {base}, but current version is {} — re-read the artifact and retry",
                        prev_meta.version
                    ),
                ));
            }
        }

        let dir = self.artifact_dir(&id);
        let unchanged = prev.is_some()
            && fs::read_to_string(dir.join(CONTENT_FILE))
                .map(|current| current == req.content)
                .unwrap_or(false);

        let version = match (&prev, unchanged) {
            (Some(m), true) => m.version,
            (Some(m), false) => m.version + 1,
            (None, _) => 1,
        };
        let mut history = prev.as_ref().map(|m| m.history.clone()).unwrap_or_default();
        if !unchanged {
            history.push(VersionInfo {
                version,
                label: req.label.clone(),
                created_at: now,
            });
        }

        let keep = |new: &Option<String>, old: fn(&ArtifactMeta) -> Option<String>| {
            new.clone().or_else(|| prev.as_ref().and_then(old))
        };
        let meta = ArtifactMeta {
            id: id.clone(),
            name: req.name.clone(),
            artifact_type: req.artifact_type.clone(),
            session_id: req.session_id.clone(),
            created_at: prev.as_ref().map(|m| m.created_at).unwrap_or(now),
            updated_at: now,
            version,
            description: keep(&req.description, |m| m.description.clone()),
            favicon: keep(&req.favicon, |m| m.favicon.clone()),
            history,
            sender: keep(&req.sender, |m| m.sender.clone()),
            recipient: keep(&req.recipient, |m| m.recipient.clone()),
            thread_id: keep(&req.thread_id, |m| m.thread_id.clone()),
            reply_to: keep(&req.reply_to, |m| m.reply_to.clone()),
            git: req
                .git
                .clone()
                .or_else(|| prev.as_ref().and_then(|m| m.git.clone())),
        };

        fs::create_dir_all(dir.join(VERSIONS_DIR))?;
        if !unchanged {
            fs::write(
                dir.join(VERSIONS_DIR).join(version.to_string()),
                gzip(req.content.as_bytes())?,
            )?;
            prune_old_versions(&dir, version, MAX_KEPT_VERSIONS);
        }
        fs::write(dir.join(META_FILE), serde_json::to_string_pretty(&meta)?)?;
        fs::write(dir.join(CONTENT_FILE), &req.content)?;

        let url = format!("/{id}");
        let response = PublishResponse {
            id,
            url,
            session_id: req.session_id.clone(),
            version,
        };

        if prev.is_some() && !unchanged {
            self.broadcast(&response.id, "refresh");
        }

        Ok(response)
    }

    /// Unified diff between two version snapshots of an artifact.
    pub fn diff(&self, id: &str, from: u32, to: u32) -> std::io::Result<String> {
        if let Some(ref store) = self.store {
            let old = self.get_version_store(store, id, from)?;
            let new = self.get_version_store(store, id, to)?;
            let diff = similar::TextDiff::from_lines(&old.content, &new.content);
            return Ok(diff
                .unified_diff()
                .header(&format!("{id} v{from}"), &format!("{id} v{to}"))
                .to_string());
        }
        let versions_dir = self.artifact_dir(id).join(VERSIONS_DIR);
        let old = read_version_file(&versions_dir.join(from.to_string()))?;
        let new = read_version_file(&versions_dir.join(to.to_string()))?;
        let diff = similar::TextDiff::from_lines(&old, &new);
        Ok(diff
            .unified_diff()
            .header(&format!("{id} v{from}"), &format!("{id} v{to}"))
            .to_string())
    }

    /// Version history, oldest first.
    pub fn versions(&self, id: &str) -> std::io::Result<Vec<VersionInfo>> {
        if let Some(ref store) = self.store {
            let doc = store
                .doc_get_by_path(DOC_PROJECT, id)
                .map_err(Self::store_conn_err)?
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("artifact {id} not found"),
                    )
                })?;
            let meta: ArtifactMeta = serde_json::from_str(&doc.metadata)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            return Ok(meta.history);
        }
        self.read_meta(id).map(|m| m.history).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("artifact {id} not found"),
            )
        })
    }

    fn get_version_store(
        &self,
        store: &Store,
        id: &str,
        version: u32,
    ) -> std::io::Result<Artifact> {
        let doc = store
            .doc_get_by_path(DOC_PROJECT, id)
            .map_err(Self::store_conn_err)?
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("artifact {id} not found"),
                )
            })?;
        let latest = Self::doc_to_artifact(&doc)?;
        if doc.version as u32 == version {
            let content = Self::blob_or_content(store, &doc)?;
            return Ok(Artifact { content, ..latest });
        }
        let hist = store.doc_history(&doc.id).map_err(Self::store_conn_err)?;
        for h in hist {
            if h.version as u32 == version {
                let content = match &h.blob_hash {
                    Some(hash) => {
                        let bytes = store
                            .blob_get(hash)
                            .map_err(Self::store_conn_err)?
                            .ok_or_else(|| {
                                std::io::Error::new(std::io::ErrorKind::NotFound, "blob not found")
                            })?;
                        String::from_utf8(bytes)
                            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
                    }
                    None => h.content,
                };
                return Ok(Artifact {
                    content,
                    version,
                    ..latest
                });
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("version {version} not found"),
        ))
    }

    fn blob_or_content(
        store: &Store,
        doc: &agentflare_store::documents::Document,
    ) -> std::io::Result<String> {
        match &doc.blob_hash {
            Some(hash) => {
                let bytes = store
                    .blob_get(hash)
                    .map_err(Self::store_conn_err)?
                    .ok_or_else(|| {
                        std::io::Error::new(std::io::ErrorKind::NotFound, "blob not found")
                    })?;
                String::from_utf8(bytes)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            }
            None => Ok(doc.content.clone()),
        }
    }

    fn get_store(&self, store: &Store, id: &str) -> std::io::Result<Artifact> {
        let doc = store
            .doc_get_by_path(DOC_PROJECT, id)
            .map_err(Self::store_conn_err)?
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("artifact {id} not found"),
                )
            })?;
        let content = Self::blob_or_content(store, &doc)?;
        let meta: ArtifactMeta = serde_json::from_str(&doc.metadata)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(Artifact {
            id: doc.path.clone(),
            name: doc.title.clone(),
            artifact_type: meta.artifact_type,
            content,
            session_id: doc.session_id.clone().unwrap_or_default(),
            created_at: doc.created_at as u64,
            updated_at: doc.updated_at as u64,
            version: doc.version as u32,
            description: meta.description,
            favicon: meta.favicon,
            sender: meta.sender,
            recipient: meta.recipient,
            thread_id: meta.thread_id,
            reply_to: meta.reply_to,
            git: meta.git,
        })
    }

    /// A specific version's snapshot; `get()` always serves the latest.
    pub fn get_version(&self, id: &str, version: u32) -> std::io::Result<Artifact> {
        if let Some(ref store) = self.store {
            return self.get_version_store(store, id, version);
        }
        let mut artifact = self.get(id)?;
        let content_path = self
            .artifact_dir(id)
            .join(VERSIONS_DIR)
            .join(version.to_string());
        artifact.content = read_version_file(&content_path)?;
        artifact.version = version;
        Ok(artifact)
    }

    pub fn get(&self, id: &str) -> std::io::Result<Artifact> {
        if let Some(ref store) = self.store {
            return self.get_store(store, id);
        }
        let dir = self.artifact_dir(id);
        if !dir.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("artifact {id} not found"),
            ));
        }
        let meta: ArtifactMeta = serde_json::from_str(&fs::read_to_string(dir.join(META_FILE))?)?;
        let content = fs::read_to_string(dir.join(CONTENT_FILE))?;
        Ok(Artifact {
            id: meta.id,
            name: meta.name,
            artifact_type: meta.artifact_type,
            content,
            session_id: meta.session_id,
            created_at: meta.created_at,
            updated_at: meta.updated_at,
            version: meta.version,
            description: meta.description,
            favicon: meta.favicon,
            sender: meta.sender,
            recipient: meta.recipient,
            thread_id: meta.thread_id,
            reply_to: meta.reply_to,
            git: meta.git,
        })
    }

    pub fn list(&self, session_id: Option<&str>) -> std::io::Result<Vec<ArtifactSummary>> {
        if let Some(ref store) = self.store {
            let docs = store.doc_list(DOC_PROJECT).map_err(Self::store_conn_err)?;
            let mut artifacts: Vec<ArtifactSummary> = docs
                .iter()
                .filter(|d| session_id.is_none_or(|sid| d.session_id.as_deref() == Some(sid)))
                .map(Self::doc_to_summary)
                .collect();
            artifacts.sort_by_key(|a| std::cmp::Reverse(a.created_at));
            return Ok(artifacts);
        }
        let dir = &self.base_path;
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut artifacts = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let meta_path = entry.path().join(META_FILE);
            if meta_path.exists() {
                if let Some(meta) = fs::read_to_string(&meta_path)
                    .ok()
                    .and_then(|s| serde_json::from_str::<ArtifactMeta>(&s).ok())
                {
                    if session_id.is_none() || meta.session_id == session_id.unwrap() {
                        artifacts.push(ArtifactSummary {
                            id: meta.id,
                            name: meta.name,
                            artifact_type: meta.artifact_type,
                            session_id: meta.session_id,
                            created_at: meta.created_at,
                            updated_at: meta.updated_at,
                            version: meta.version,
                            description: meta.description,
                            favicon: meta.favicon,
                            sender: meta.sender,
                            recipient: meta.recipient,
                            thread_id: meta.thread_id,
                            reply_to: meta.reply_to,
                        });
                    }
                }
            }
        }
        artifacts.sort_by_key(|a| std::cmp::Reverse(a.created_at));
        Ok(artifacts)
    }

    pub fn delete(&self, id: &str) -> std::io::Result<bool> {
        if let Some(ref store) = self.store {
            let doc = store
                .doc_get_by_path(DOC_PROJECT, id)
                .map_err(Self::store_conn_err)?;
            return match doc {
                Some(d) => store.doc_delete(&d.id).map_err(Self::store_conn_err),
                None => Ok(false),
            };
        }
        let dir = self.artifact_dir(id);
        if !dir.exists() {
            return Ok(false);
        }
        fs::remove_dir_all(dir)?;
        Ok(true)
    }

    pub fn subscribe(&self, id: &str) -> std::sync::mpsc::Receiver<String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut subs = self.live_broadcast.lock().unwrap();
        subs.entry(id.to_string()).or_default().push(tx);
        rx
    }

    fn broadcast(&self, id: &str, event: &str) {
        let mut subs = self.live_broadcast.lock().unwrap();
        if let Some(senders) = subs.remove(id) {
            for tx in senders {
                let _ = tx.send(event.to_string());
            }
        }
    }

    fn artifact_dir(&self, id: &str) -> PathBuf {
        self.base_path.join(id)
    }

    fn read_meta(&self, id: &str) -> Option<ArtifactMeta> {
        let path = self.artifact_dir(id).join(META_FILE);
        fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    pub fn base_path(&self) -> &Path {
        &self.base_path
    }
}

/// Delete version-snapshot bodies older than the `keep` most-recent, always
/// preserving v1 as the origin anchor. Oldest-first, no-op once already
/// under the cap — safe to call unconditionally on every publish.
fn prune_old_versions(dir: &Path, latest_version: u32, keep: u32) {
    if latest_version <= keep {
        return;
    }
    let cutoff = latest_version - keep;
    let versions_dir = dir.join(VERSIONS_DIR);
    for v in 2..=cutoff {
        let _ = fs::remove_file(versions_dir.join(v.to_string()));
    }
}

fn gzip(data: &[u8]) -> std::io::Result<Vec<u8>> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data)?;
    enc.finish()
}

/// Reads a version-snapshot file, transparently decompressing it if it's
/// gzip (2-byte magic `1f 8b`). A snapshot written before compression
/// landed has no such header, so it's read as plain UTF-8 text unchanged —
/// self-describing, no version marker or migration needed to keep serving
/// snapshots written by older builds.
fn read_version_file(path: &Path) -> std::io::Result<String> {
    let data = fs::read(path)?;
    let bytes = if data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b {
        use flate2::read::GzDecoder;
        use std::io::Read;
        let mut out = Vec::new();
        GzDecoder::new(&data[..]).read_to_end(&mut out)?;
        out
    } else {
        data
    };
    String::from_utf8(bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, ArtifactStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(tmp.path().to_path_buf());
        (tmp, store)
    }

    fn doc_store() -> (tempfile::TempDir, ArtifactStore) {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("store.db");
        let s = agentflare_store::Store::open_file(&db_path).unwrap();
        let store = ArtifactStore::with_store(s);
        (tmp, store)
    }

    fn publish(store: &ArtifactStore, update_id: Option<String>, content: &str) -> String {
        store
            .publish(&PublishRequest {
                name: "doc".into(),
                artifact_type: ArtifactType::Markdown,
                content: content.into(),
                session_id: "s1".into(),
                update_id,
                ..Default::default()
            })
            .unwrap()
            .id
    }

    #[test]
    fn prune_old_versions_is_a_noop_under_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(VERSIONS_DIR)).unwrap();
        for v in 1..=5u32 {
            fs::write(dir.path().join(VERSIONS_DIR).join(v.to_string()), "x").unwrap();
        }
        prune_old_versions(dir.path(), 5, 50);
        for v in 1..=5u32 {
            assert!(dir.path().join(VERSIONS_DIR).join(v.to_string()).exists());
        }
    }

    #[test]
    fn prune_old_versions_drops_the_old_tail_but_keeps_v1_and_recent() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(VERSIONS_DIR)).unwrap();
        for v in 1..=10u32 {
            fs::write(dir.path().join(VERSIONS_DIR).join(v.to_string()), "x").unwrap();
        }
        prune_old_versions(dir.path(), 10, 3);
        let exists = |v: u32| dir.path().join(VERSIONS_DIR).join(v.to_string()).exists();
        assert!(exists(1), "v1 is always kept as the origin anchor");
        for v in 2..=7u32 {
            assert!(
                !exists(v),
                "v{v} is older than the last 3 kept and should be pruned"
            );
        }
        for v in 8..=10u32 {
            assert!(exists(v), "v{v} is within the last 3 kept");
        }
    }

    #[test]
    fn publish_loop_prunes_old_version_bodies_but_keeps_full_history() {
        let (_tmp, store) = store();
        let id = publish(&store, None, "v1");
        let mut last_id = id.clone();
        for i in 2..=60 {
            last_id = publish(&store, Some(last_id), &format!("v{i}"));
        }

        // history (the version list) still shows every publish...
        let history = store.versions(&id).unwrap();
        assert_eq!(history.len(), 60);

        // ...but old version bodies beyond the cap are gone from disk, while
        // v1 and the recent tail survive.
        assert!(store.diff(&id, 1, 2).is_err(), "v2 body pruned by now");
        assert!(store.get_version(&id, 1).is_ok(), "v1 anchor kept");
        assert!(store.get_version(&id, 60).is_ok(), "latest version kept");
    }

    #[test]
    fn version_snapshots_are_stored_gzip_compressed() {
        let (_tmp, store) = store();
        let content = "repeat me ".repeat(200);
        let id = publish(&store, None, &content);

        let raw = fs::read(store.artifact_dir(&id).join(VERSIONS_DIR).join("1")).unwrap();
        assert!(
            raw.len() < content.len(),
            "on-disk snapshot ({} bytes) should be smaller than the source ({} bytes)",
            raw.len(),
            content.len()
        );
        assert_eq!(&raw[..2], &[0x1f, 0x8b], "gzip magic header");

        assert_eq!(store.get_version(&id, 1).unwrap().content, content);
    }

    #[test]
    fn a_legacy_plaintext_version_written_before_gzip_still_reads_back() {
        let (_tmp, store) = store();
        let id = publish(&store, None, "v1");

        // Simulate a snapshot written by a build predating compression: no
        // gzip magic, plain UTF-8 text on disk.
        fs::write(
            store.artifact_dir(&id).join(VERSIONS_DIR).join("1"),
            "legacy plaintext, no gzip header",
        )
        .unwrap();

        assert_eq!(
            store.get_version(&id, 1).unwrap().content,
            "legacy plaintext, no gzip header"
        );
    }

    // ── store-backed (agentflare_store) tests ──

    #[test]
    fn doc_store_publish_and_get() {
        let (_tmp, store) = doc_store();
        assert!(store.has_store());
        let id = publish(&store, None, "hello from doc store");
        let artifact = store.get(&id).unwrap();
        assert_eq!(artifact.content, "hello from doc store");
        assert_eq!(artifact.version, 1);
    }

    #[test]
    fn doc_store_update_increments_version() {
        let (_tmp, store) = doc_store();
        let id = publish(&store, None, "v1");
        assert_eq!(store.get(&id).unwrap().version, 1);

        let id2 = publish(&store, Some(id.clone()), "v2");
        assert_eq!(id, id2);
        assert_eq!(store.get(&id).unwrap().content, "v2");
        assert_eq!(store.get(&id).unwrap().version, 2);
    }

    #[test]
    fn doc_store_versions_and_history() {
        let (_tmp, store) = doc_store();
        let id = publish(&store, None, "v1");
        publish(&store, Some(id.clone()), "v2");
        publish(&store, Some(id.clone()), "v3");

        let history = store.versions(&id).unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].version, 1);
        assert_eq!(history[1].version, 2);
        assert_eq!(history[2].version, 3);

        assert_eq!(store.get_version(&id, 1).unwrap().content, "v1");
        assert_eq!(store.get_version(&id, 2).unwrap().content, "v2");
        assert_eq!(store.get_version(&id, 3).unwrap().content, "v3");
    }

    #[test]
    fn doc_store_list_by_session() {
        let (_tmp, store) = doc_store();
        let s1 = PublishRequest {
            name: "s1-doc".into(),
            artifact_type: ArtifactType::Text,
            content: "a".into(),
            session_id: "session-A".into(),
            ..Default::default()
        };
        let s2 = PublishRequest {
            name: "s2-doc".into(),
            artifact_type: ArtifactType::Text,
            content: "b".into(),
            session_id: "session-B".into(),
            ..Default::default()
        };
        store.publish(&s1).unwrap();
        store.publish(&s2).unwrap();

        let all = store.list(None).unwrap();
        assert_eq!(all.len(), 2);

        let only_a = store.list(Some("session-A")).unwrap();
        assert_eq!(only_a.len(), 1);
        assert_eq!(only_a[0].name, "s1-doc");

        let only_b = store.list(Some("session-B")).unwrap();
        assert_eq!(only_b.len(), 1);
    }

    #[test]
    fn doc_store_delete() {
        let (_tmp, store) = doc_store();
        let id = publish(&store, None, "to-delete");
        assert!(store.get(&id).is_ok());
        assert!(store.delete(&id).unwrap());
        assert!(store.get(&id).is_err());
        // second delete returns false
        assert!(!store.delete(&id).unwrap());
    }

    #[test]
    fn doc_store_diff_between_versions() {
        let (_tmp, store) = doc_store();
        let id = publish(&store, None, "line one\nline two\n");
        publish(&store, Some(id.clone()), "line one\nline two modified\n");

        let d = store.diff(&id, 1, 2).unwrap();
        assert!(d.contains("line two"));
        assert!(d.contains("line two modified"));
    }
}
