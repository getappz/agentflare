use async_trait::async_trait;

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

#[async_trait]
pub trait Fetcher: Send + Sync {
    async fn fetch(&self, url: &str) -> Result<FetchedBytes, FetchError>;
}

pub fn decompress_zstd(bytes: &[u8]) -> Result<Vec<u8>, FetchError> {
    zstd::stream::decode_all(bytes).map_err(|e| FetchError::Decompress(e.to_string()))
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

pub struct ReqwestFetcher {
    client: reqwest::Client,
}

impl ReqwestFetcher {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Fetcher for ReqwestFetcher {
    async fn fetch(&self, url: &str) -> Result<FetchedBytes, FetchError> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| FetchError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(FetchError::Http(format!("status {}", resp.status())));
        }
        let etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| FetchError::Http(e.to_string()))?
            .to_vec();
        Ok(FetchedBytes {
            bytes,
            etag,
            content_type,
        })
    }
}
