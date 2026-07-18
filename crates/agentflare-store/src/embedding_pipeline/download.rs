use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::model_registry::ModelConfig;

const USER_AGENT: &str = concat!("agentflare-store/", env!("CARGO_PKG_VERSION"));
const LOCKFILE: &str = "model.lock.json";

struct DownloadFile {
    url: String,
    local_name: String,
    min_bytes: u64,
}

pub fn ensure_model(model_dir: &Path, config: &ModelConfig) -> anyhow::Result<PathBuf> {
    let files = download_files(config);

    // Re-verify already-present files (size + SHA-256 against the pinned hash in
    // model.lock.json) before trusting them. A present-but-corrupt file (partial
    // write, disk corruption, tampering) must not silently bypass the checksum,
    // so a size/hash mismatch is treated as missing and re-downloaded below.
    let lock = read_lockfile(model_dir)?;
    let mut any_corrupt = false;
    for file in &files {
        let local_path = model_dir.join(&file.local_name);
        if !local_path.exists() {
            continue;
        }
        if !file_passes_verification(&local_path, file, &lock)? {
            tracing::warn!(
                "Embedding model file {} present but failed size/SHA-256 verification; re-downloading",
                file.local_name
            );
            std::fs::remove_file(&local_path).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to remove corrupt model file {}: {e}. Refusing to \
                     continue with unverified data left on disk.",
                    local_path.display()
                )
            })?;
            any_corrupt = true;
        }
    }

    let all_present = files.iter().all(|f| model_dir.join(&f.local_name).exists());
    if all_present && !any_corrupt {
        return Ok(model_dir.to_path_buf());
    }

    tracing::info!(
        "Embedding model '{}' not found or invalid, downloading to {}",
        config.name,
        model_dir.display()
    );
    std::fs::create_dir_all(model_dir)?;

    let mut lock = read_lockfile(model_dir)?;

    for file in &files {
        let local_path = model_dir.join(&file.local_name);
        if local_path.exists() {
            let meta = std::fs::metadata(&local_path)?;
            if meta.len() >= file.min_bytes {
                continue;
            }
        }
        download_file(&file.url, &file.local_name, file.min_bytes, model_dir)?;
        let actual = sha256_file(&model_dir.join(&file.local_name))?;
        match lock.get(&file.local_name) {
            Some(pinned) if pinned != &actual => {
                let _ = std::fs::remove_file(model_dir.join(&file.local_name));
                anyhow::bail!(
                    "SHA-256 mismatch for {} of model '{}': pinned {pinned}, got {actual}. \
                     Upstream content changed under same revision. Delete {} and re-download.",
                    file.local_name,
                    config.name,
                    model_dir.join(LOCKFILE).display()
                );
            }
            Some(_) => {}
            None => {
                lock.insert(file.local_name.clone(), actual);
            }
        }
    }

    write_lockfile(model_dir, &lock)?;
    tracing::info!(
        "Embedding model '{}' ready at {}",
        config.name,
        model_dir.display()
    );
    Ok(model_dir.to_path_buf())
}

fn download_files(config: &ModelConfig) -> Vec<DownloadFile> {
    vec![
        DownloadFile {
            url: config.model_url(),
            local_name: "model.onnx".to_string(),
            min_bytes: config.model_min_bytes,
        },
        DownloadFile {
            url: config.vocab_url(),
            local_name: config.vocab_file.filename().to_string(),
            min_bytes: config.vocab_min_bytes,
        },
    ]
}

fn download_file(
    url: &str,
    local_name: &str,
    min_bytes: u64,
    model_dir: &Path,
) -> anyhow::Result<()> {
    let local_path = model_dir.join(local_name);
    let tmp_path = model_dir.join(format!("{local_name}.tmp"));

    tracing::info!("Downloading {local_name} ...");

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(30))
        .timeout_read(Duration::from_secs(300))
        .build();

    let response = agent
        .get(url)
        .set("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| anyhow::anyhow!("Failed to download {url}: {e}"))?;

    let status = response.status();
    if status != 200 {
        anyhow::bail!("Download of {local_name} returned HTTP {status}");
    }

    let mut body = response.into_reader();
    let mut out = std::fs::File::create(&tmp_path)?;
    let mut buf = vec![0u8; 65536];
    let mut total: u64 = 0;

    loop {
        let n = body.read(&mut buf)?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut out, &buf[..n])?;
        total += n as u64;
    }
    drop(out);

    if total < min_bytes {
        let _ = std::fs::remove_file(&tmp_path);
        anyhow::bail!(
            "Downloaded {local_name} is too small ({total} bytes, expected >= {min_bytes})"
        );
    }

    std::fs::rename(&tmp_path, &local_path)?;
    tracing::info!("  {local_name} — {:.1}MB saved", total as f64 / 1_048_576.0);
    Ok(())
}

/// Re-verify a present model file: it must meet the minimum size and, when a
/// pinned SHA-256 exists in the lockfile, match that hash. Returns `true` when
/// the file can be trusted as-is (so `ensure_model` need not re-download it).
fn file_passes_verification(
    local_path: &Path,
    file: &DownloadFile,
    lock: &BTreeMap<String, String>,
) -> anyhow::Result<bool> {
    let meta = std::fs::metadata(local_path)?;
    if meta.len() < file.min_bytes {
        return Ok(false);
    }
    let actual = sha256_file(local_path)?;
    match lock.get(&file.local_name) {
        Some(pinned) if pinned != &actual => Ok(false),
        _ => Ok(true),
    }
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let result = hasher.finalize();
    Ok(format!("{result:x}"))
}

/// A missing lockfile is expected on first run (`Ok(empty)`); an existing but
/// unreadable or malformed one is not — silently treating it as empty would
/// make every present file look unpinned and skip verification entirely.
fn read_lockfile(model_dir: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    let path = model_dir.join(LOCKFILE);
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s)
            .map_err(|e| anyhow::anyhow!("Malformed lockfile {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(anyhow::anyhow!(
            "Cannot read lockfile {}: {e}",
            path.display()
        )),
    }
}

fn write_lockfile(model_dir: &Path, lock: &BTreeMap<String, String>) -> anyhow::Result<()> {
    if lock.is_empty() {
        return Ok(());
    }
    let json = serde_json::to_string_pretty(lock)?;
    std::fs::write(model_dir.join(LOCKFILE), json)?;
    Ok(())
}

pub fn clean_model(model_dir: &Path) -> anyhow::Result<()> {
    for name in ["model.onnx", "vocab.txt", "tokenizer.json", LOCKFILE] {
        let path = model_dir.join(name);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        let tmp_path = model_dir.join(format!("{name}.tmp"));
        if tmp_path.exists() {
            std::fs::remove_file(&tmp_path)?;
        }
    }
    Ok(())
}

#[cfg(all(test, feature = "embeddings"))]
mod tests {
    use super::*;
    use crate::embedding_pipeline::model_registry::{EmbeddingModel, ModelConfig};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const MODEL_BODY: &[u8] = b"MODEL-FILE-CONTENTS-0123456789-abcdefghij";
    const VOCAB_BODY: &[u8] = b"vocab line one\nvocab line two\nvocab line three\n";

    /// Serves two fixed files from a local HTTP server so the regression test
    /// needs no network and produces deterministic SHA-256 hashes.
    fn spawn_server() -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits2 = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let body: &[u8] = if buf.starts_with(b"GET /onnx/model.onnx") {
                    MODEL_BODY
                } else {
                    VOCAB_BODY
                };
                hits2.fetch_add(1, Ordering::SeqCst);
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(body);
                let _ = stream.flush();
            }
        });
        (format!("http://127.0.0.1:{port}"), hits)
    }

    fn test_config(base: &str) -> ModelConfig {
        let mut cfg = EmbeddingModel::AllMiniLmL6V2.config();
        cfg.base_url_override = Some(base.to_string());
        cfg.model_min_bytes = 1;
        cfg.vocab_min_bytes = 1;
        cfg
    }

    fn sha256(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(bytes);
        format!("{:x}", h.finalize())
    }

    #[test]
    fn present_file_reverified_on_hash_mismatch() {
        let (base, hits) = spawn_server();
        let dir = tempfile::tempdir().unwrap();
        let model_dir = dir.path().join("model");
        let config = test_config(&base);

        // First call downloads and pins both files.
        ensure_model(&model_dir, &config).unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert_eq!(
            std::fs::read(model_dir.join("model.onnx")).unwrap(),
            MODEL_BODY
        );
        assert_eq!(
            std::fs::read(model_dir.join("vocab.txt")).unwrap(),
            VOCAB_BODY
        );

        // Tamper with the vocab file so its SHA-256 no longer matches the pin.
        std::fs::write(
            model_dir.join("vocab.txt"),
            b"CORRUPTED-VOCAB-DATA-NOT-REAL",
        )
        .unwrap();

        // Second call must detect the mismatch and re-download the file.
        ensure_model(&model_dir, &config).unwrap();

        // Re-downloaded file now matches the genuine server content.
        assert_eq!(
            std::fs::read(model_dir.join("vocab.txt")).unwrap(),
            VOCAB_BODY
        );

        // The lockfile still records the genuine pinned hashes.
        let lock: std::collections::BTreeMap<String, String> = serde_json::from_str(
            &std::fs::read_to_string(model_dir.join("model.lock.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(lock.get("vocab.txt").unwrap(), &sha256(VOCAB_BODY));
        assert_eq!(lock.get("model.onnx").unwrap(), &sha256(MODEL_BODY));
        assert!(hits.load(Ordering::SeqCst) >= 3);
    }

    #[test]
    fn present_file_reverified_on_size_mismatch() {
        let (base, _hits) = spawn_server();
        let dir = tempfile::tempdir().unwrap();
        let model_dir = dir.path().join("model");
        let config = test_config(&base);

        ensure_model(&model_dir, &config).unwrap();

        // Truncate the vocab file below the minimum size.
        std::fs::write(model_dir.join("vocab.txt"), b"").unwrap();

        ensure_model(&model_dir, &config).unwrap();
        assert_eq!(
            std::fs::read(model_dir.join("vocab.txt")).unwrap(),
            VOCAB_BODY
        );
    }
}
