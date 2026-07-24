use std::io::Read;

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("http error: {0}")]
    Http(String),
    #[error("decompression error: {0}")]
    Decompress(String),
}

#[derive(Debug, Clone)]
pub struct FetchedBytes {
    pub bytes: Vec<u8>,
    pub etag: Option<String>,
    pub content_type: Option<String>,
}

pub trait Fetcher: Send + Sync {
    fn fetch(&self, url: &str) -> Result<FetchedBytes, FetchError>;
}

pub fn decompress_zstd(bytes: &[u8]) -> Result<Vec<u8>, FetchError> {
    zstd::stream::decode_all(bytes).map_err(|e| FetchError::Decompress(e.to_string()))
}

const USER_AGENT: &str = concat!("flare-docs/", env!("CARGO_PKG_VERSION"));

pub struct UreqFetcher {
    agent: ureq::Agent,
}

impl UreqFetcher {
    pub fn new() -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout_connect(std::time::Duration::from_secs(30))
                .timeout_read(std::time::Duration::from_secs(300))
                .build(),
        }
    }
}

impl Default for UreqFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetcher for UreqFetcher {
    fn fetch(&self, url: &str) -> Result<FetchedBytes, FetchError> {
        let resp = self
            .agent
            .get(url)
            .set("User-Agent", USER_AGENT)
            .call()
            .map_err(|e| FetchError::Http(e.to_string()))?;

        let status = resp.status();
        if !(200..300).contains(&status) {
            return Err(FetchError::Http(format!("status {status}")));
        }

        let etag = resp.header("etag").map(|s| s.to_string());
        let content_type = resp.header("content-type").map(|s| s.to_string());

        let mut bytes = Vec::new();
        resp.into_reader()
            .read_to_end(&mut bytes)
            .map_err(|e| FetchError::Http(e.to_string()))?;

        Ok(FetchedBytes {
            bytes,
            etag,
            content_type,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decompress_zstd_round_trip() {
        let original = b"hello rustdoc json world";
        let compressed = zstd::stream::encode_all(&original[..], 0).unwrap();
        let decompressed = decompress_zstd(&compressed).unwrap();
        assert_eq!(decompressed, original);
    }

    #[test]
    fn decompress_zstd_rejects_garbage() {
        let garbage = b"not zstd data at all";
        let result = decompress_zstd(garbage);
        assert!(result.is_err());
    }
}
