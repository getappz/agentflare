use crate::fetch::{FetchError, FetchedBytes, Fetcher, decompress_zstd};
use crate::store::{DocsStore, Error as StoreError};
use agentflare_store::documents::{DocUpsertOpts, Document};

#[derive(Debug, thiserror::Error)]
pub enum RustdocError {
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("invalid rustdoc json: {0}")]
    InvalidJson(String),
}

/// docs.rs's official rustdoc-JSON endpoint (RFC 2963). Verified live
/// 2026-07-23: both `latest` and an exact semver return HTTP 200,
/// `content-type: application/zstd`. version may be "latest" or an exact
/// version string (e.g. "1.0.229").
pub fn docs_rs_json_url(crate_name: &str, version: &str) -> String {
    format!("https://docs.rs/crate/{crate_name}/{version}/json")
}

pub fn extract_root_docstring(json_bytes: &[u8]) -> Result<Option<String>, RustdocError> {
    let value: serde_json::Value =
        serde_json::from_slice(json_bytes).map_err(|e| RustdocError::InvalidJson(e.to_string()))?;
    let root_value = value
        .get("root")
        .ok_or_else(|| RustdocError::InvalidJson("missing \"root\" field".to_string()))?;
    // Real docs.rs rustdoc-JSON output (format_version 60) encodes `root` as a
    // JSON number (e.g. `3177`), while some synthetic fixtures use a string
    // (e.g. `"0:0"`). `index`'s keys are always JSON object keys, i.e.
    // strings, so a numeric root must be stringified before the lookup.
    let root_key = match root_value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        other => {
            return Err(RustdocError::InvalidJson(format!(
                "\"root\" field has unexpected type: {other:?}"
            )));
        }
    };
    let docs = value
        .get("index")
        .and_then(|idx| idx.get(&root_key))
        .and_then(|item| item.get("docs"))
        .and_then(|d| d.as_str())
        .map(|s| s.to_string());
    Ok(docs)
}

pub fn fetch_and_store(
    fetcher: &dyn Fetcher,
    store: &DocsStore,
    crate_name: &str,
    version: &str,
) -> Result<Document, RustdocError> {
    let url = docs_rs_json_url(crate_name, version);
    let fetched = fetcher.fetch(&url)?;
    store_fetched(store, &fetched, crate_name, version)
}

/// Processes already-fetched rustdoc-JSON bytes (decompress, parse, store).
///
/// Split out from [`fetch_and_store`] so callers that need the network fetch
/// to happen off the calling thread (e.g. the MCP server offloading it to
/// `tokio::task::spawn_blocking`) can run the fetch alone, `.await` it, and
/// only then do this fast local work — without ever holding a store lock (or
/// any lock) across the blocking network call.
pub fn store_fetched(
    store: &DocsStore,
    fetched: &FetchedBytes,
    crate_name: &str,
    version: &str,
) -> Result<Document, RustdocError> {
    let decompressed = decompress_zstd(&fetched.bytes)?;
    let docstring = extract_root_docstring(&decompressed)?
        .unwrap_or_else(|| format!("(no crate-level documentation for {crate_name})"));

    let opts = DocUpsertOpts {
        title: Some(crate_name.to_string()),
        doc_type: Some("rust-crate".to_string()),
        source: Some("docsrs".to_string()),
        tags: Some(vec![crate_name.to_string(), "rust".to_string()]),
        blob_hash: Some(store_raw_json_blob(store, &decompressed)?),
        ..Default::default()
    };
    let id_path = format!("docsrs/{crate_name}/{version}");
    Ok(store.upsert(&id_path, &docstring, opts)?)
}

fn store_raw_json_blob(store: &DocsStore, decompressed_json: &[u8]) -> Result<String, StoreError> {
    store.blob_store_raw(decompressed_json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_verified_url_shape() {
        assert_eq!(
            docs_rs_json_url("serde", "latest"),
            "https://docs.rs/crate/serde/latest/json"
        );
        assert_eq!(
            docs_rs_json_url("tokio", "1.40.0"),
            "https://docs.rs/crate/tokio/1.40.0/json"
        );
    }

    #[test]
    fn extracts_root_docstring_from_minimal_fixture() {
        let fixture = br#"{
            "root": "0:0",
            "index": {
                "0:0": {
                    "docs": "A generic serialization/deserialization framework."
                }
            }
        }"#;
        let docs = extract_root_docstring(fixture).unwrap();
        assert_eq!(
            docs,
            Some("A generic serialization/deserialization framework.".to_string())
        );
    }

    #[test]
    fn extracts_none_when_root_docstring_is_null() {
        let fixture = br#"{
            "root": "0:0",
            "index": { "0:0": { "docs": null } }
        }"#;
        let docs = extract_root_docstring(fixture).unwrap();
        assert_eq!(docs, None);
    }

    #[test]
    fn errors_on_missing_root_field() {
        let fixture = br#"{ "index": {} }"#;
        let result = extract_root_docstring(fixture);
        assert!(result.is_err());
    }

    #[test]
    fn extracts_root_docstring_when_root_is_a_json_number() {
        let fixture = br#"{
            "root": 3177,
            "index": {
                "3177": { "docs": "A generic serialization/deserialization framework." }
            }
        }"#;
        let docs = extract_root_docstring(fixture).unwrap();
        assert_eq!(
            docs,
            Some("A generic serialization/deserialization framework.".to_string())
        );
    }

    use crate::fetch::FetchedBytes;

    struct FakeFetcher {
        response: Vec<u8>,
    }

    impl Fetcher for FakeFetcher {
        fn fetch(&self, _url: &str) -> Result<FetchedBytes, FetchError> {
            Ok(FetchedBytes {
                bytes: self.response.clone(),
                etag: Some("\"fake-etag\"".to_string()),
                content_type: Some("application/zstd".to_string()),
            })
        }
    }

    #[test]
    fn fetch_and_store_persists_the_docstring() {
        let raw_json = br#"{
            "root": "0:0",
            "index": { "0:0": { "docs": "A fake crate for testing." } }
        }"#;
        let compressed = zstd::stream::encode_all(&raw_json[..], 0).unwrap();
        let fetcher = FakeFetcher {
            response: compressed,
        };
        let store = DocsStore::open_memory().unwrap();

        let doc = fetch_and_store(&fetcher, &store, "fake-crate", "1.0.0").unwrap();

        assert_eq!(doc.content, "A fake crate for testing.");
        assert_eq!(doc.path, "docsrs/fake-crate/1.0.0");
        assert_eq!(doc.doc_type, "rust-crate");
        assert!(doc.blob_hash.is_some());

        let hits = store.search("fake crate", 10).unwrap();
        assert_eq!(hits.len(), 1);
    }
}
