use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::model_registry::{ModelConfig, VocabSource};

const USER_AGENT: &str = concat!("agentflare-store/", env!("CARGO_PKG_VERSION"));
const LOCKFILE: &str = "model.lock.json";

struct DownloadFile {
    url: String,
    local_name: String,
    min_bytes: u64,
}

pub fn ensure_model(model_dir: &Path, config: &ModelConfig) -> anyhow::Result<PathBuf> {
    let files = download_files(config);
    let all_present = files.iter().all(|f| model_dir.join(&f.local_name).exists());

    if all_present {
        return Ok(model_dir.to_path_buf());
    }

    tracing::info!(
        "Embedding model '{}' not found, downloading to {}",
        config.name,
        model_dir.display()
    );
    std::fs::create_dir_all(model_dir)?;

    let mut lock = read_lockfile(model_dir);

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
                    file.local_name, config.name,
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
    tracing::info!("Embedding model '{}' ready at {}", config.name, model_dir.display());
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
        anyhow::bail!("Downloaded {local_name} is too small ({total} bytes, expected >= {min_bytes})");
    }

    std::fs::rename(&tmp_path, &local_path)?;
    tracing::info!("  {local_name} — {:.1}MB saved", total as f64 / 1_048_576.0);
    Ok(())
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

fn read_lockfile(model_dir: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(model_dir.join(LOCKFILE))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
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
