use std::io::Read;

/// docs.rs rustdoc-JSON payloads for real-world crates run from a few KB to
/// low tens of MB compressed; these caps are generous headroom over that,
/// not a tight fit — they exist to bound memory use against a compromised or
/// misbehaving response, not to reject legitimate payloads.
const MAX_COMPRESSED_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DECOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("http error: {0}")]
    Http(String),
    #[error("decompression error: {0}")]
    Decompress(String),
    #[error("response too large: {0}")]
    TooLarge(String),
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

/// Reads at most `max + 1` bytes so an oversized source is detected without
/// buffering the whole (potentially unbounded) stream first.
fn read_capped(reader: impl Read, max: u64) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    reader.take(max + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > max {
        return Err(std::io::Error::other(format!("exceeded {max} byte limit")));
    }
    Ok(buf)
}

pub fn decompress_zstd(bytes: &[u8]) -> Result<Vec<u8>, FetchError> {
    let decoder = zstd::stream::read::Decoder::new(bytes)
        .map_err(|e| FetchError::Decompress(e.to_string()))?;
    read_capped(decoder, MAX_DECOMPRESSED_BYTES).map_err(|e| FetchError::TooLarge(e.to_string()))
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

        let bytes = read_capped(resp.into_reader(), MAX_COMPRESSED_BYTES)
            .map_err(|e| FetchError::TooLarge(e.to_string()))?;

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

    #[test]
    fn read_capped_rejects_input_over_the_limit() {
        let data = [0u8; 101];
        assert!(read_capped(&data[..], 100).is_err());
    }

    #[test]
    fn read_capped_allows_input_at_exactly_the_limit() {
        let data = [0u8; 100];
        let read = read_capped(&data[..], 100).unwrap();
        assert_eq!(read.len(), 100);
    }

    #[test]
    fn decompress_zstd_rejects_output_over_the_limit() {
        // A payload whose decompressed size alone exceeds
        // MAX_DECOMPRESSED_BYTES must be rejected without ever fully
        // materializing in memory.
        let huge = vec![0u8; (MAX_DECOMPRESSED_BYTES + 1) as usize];
        let compressed = zstd::stream::encode_all(&huge[..], 0).unwrap();
        drop(huge);
        let result = decompress_zstd(&compressed);
        assert!(matches!(result, Err(FetchError::TooLarge(_))));
    }
}
