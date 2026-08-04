use crate::fetch::{FetchError, FetchedBytes, Fetcher, decompress_zstd};
use crate::index_types::{IndexCrate, indexed_items};
use crate::store::{BatchItem, DocsStore, Error as StoreError};
use agentflare_store::documents::DocUpsertOpts;

#[derive(Debug, thiserror::Error)]
pub enum RustdocError {
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("invalid rustdoc json: {0}")]
    InvalidJson(String),
}

/// Result of [`fetch_and_store`]/[`store_fetched`]: the crate-overview doc,
/// plus a visible record of whether per-item indexing succeeded. Per-item
/// indexing is best-effort on top of the overview doc (see [`store_fetched`]
/// for why a schema-parse failure there must not fail the whole fetch), but
/// that must not mean invisible -- previously a failure there only reached
/// an `eprintln!` on the MCP server's own stderr, which no caller (CLI,
/// MCP client, tests) could ever observe.
/// Defined at the crate root because both ecosystems report fetches this way;
/// re-exported here so existing `rustdoc::FetchOutcome` paths keep resolving.
pub use crate::FetchOutcome;

/// docs.rs's official rustdoc-JSON endpoint (RFC 2963). Verified live
/// 2026-07-23: both `latest` and an exact semver return HTTP 200,
/// `content-type: application/zstd`. version may be "latest" or an exact
/// version string (e.g. "1.0.229").
pub fn docs_rs_json_url(crate_name: &str, version: &str) -> String {
    format!("https://docs.rs/crate/{crate_name}/{version}/json")
}

/// The [`DocsStore`] path a package/version's fetched docs are cached under.
pub fn docs_id_path(crate_name: &str, version: &str) -> String {
    format!("docsrs/{crate_name}/{version}")
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
    // A missing index entry, or a "docs" field that's absent or the wrong
    // type, is a malformed/unexpected response — treat it as an error, not
    // as "no docs" (Value::Null). Conflating the two would let a malformed
    // response silently overwrite a previously-cached, valid docstring with
    // an empty placeholder on the next fetch_and_store.
    let item = value
        .get("index")
        .and_then(|idx| idx.get(&root_key))
        .ok_or_else(|| {
            RustdocError::InvalidJson(format!("missing root item {root_key:?} in \"index\""))
        })?;
    let docs = match item.get("docs") {
        Some(serde_json::Value::Null) | None => None,
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(other) => {
            return Err(RustdocError::InvalidJson(format!(
                "root item \"docs\" has unexpected type: {other:?}"
            )));
        }
    };
    Ok(docs)
}

pub fn fetch_and_store(
    fetcher: &dyn Fetcher,
    store: &DocsStore,
    crate_name: &str,
    version: &str,
) -> Result<FetchOutcome, RustdocError> {
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
) -> Result<FetchOutcome, RustdocError> {
    let decompressed = decompress_zstd(&fetched.bytes)?;
    let docstring = extract_root_docstring(&decompressed)?
        .unwrap_or_else(|| format!("(no crate-level documentation for {crate_name})"));

    let opts = DocUpsertOpts {
        title: Some(crate_name.to_string()),
        doc_type: Some("rust-crate".to_string()),
        source: Some("docsrs".to_string()),
        tags: Some(vec![crate_name.to_string(), "rust".to_string()]),
        blob_hash: Some(store_raw_json_blob(store, &decompressed)?),
        track_history: false,
        ..Default::default()
    };
    let id_path = docs_id_path(crate_name, version);
    let overview_doc = store.upsert(&id_path, &docstring, opts)?;

    // Per-item indexing is a best-effort enhancement on top of the
    // crate-overview doc above, which has already succeeded. A rustdoc-json
    // schema mismatch on some items (e.g. an `ItemKind` variant newer than
    // this crate's pinned `rustdoc-types` version recognizes) fails
    // `IndexCrate`'s deserialization for the *entire* crate's item set --
    // that must not turn an otherwise-successful fetch into a reported
    // failure, or mask the overview doc's success from the caller. It must
    // not be invisible either, though -- surface it in `FetchOutcome` (not
    // an `eprintln!`, which a library shouldn't own; the CLI and MCP
    // callers decide what to do with `items_error`) so a caller relying on
    // per-item search results can tell "nothing changed" apart from
    // "indexing silently failed".
    let (items_indexed, items_error) = match index_items(store, &decompressed, crate_name, &id_path)
    {
        Ok(n) => (n, None),
        Err(e) => (0, Some(e.to_string())),
    };

    Ok(FetchOutcome {
        doc: overview_doc,
        items_indexed,
        items_error,
    })
}

/// Parses the full rustdoc-JSON `index`/`paths` maps and upserts one
/// document per item that has both a docstring and a public path (see
/// [`indexed_items`]). Runs as a single batched transaction via
/// [`DocsStore::upsert_batch_reconciled`] — a crate can have thousands of
/// items, and upserting them one at a time would turn a single fetch into
/// thousands of individual commits. Reconciling against `crate_id_path`'s
/// existing items also soft-deletes any item that was present in a
/// previous fetch but isn't in this one (renamed, removed, or made
/// private), so search can't keep surfacing docs for an item that no
/// longer exists in the crate's current index.
fn index_items(
    store: &DocsStore,
    decompressed_json: &[u8],
    crate_name: &str,
    crate_id_path: &str,
) -> Result<usize, RustdocError> {
    let index_crate: IndexCrate = serde_json::from_slice(decompressed_json)
        .map_err(|e| RustdocError::InvalidJson(e.to_string()))?;

    let item_prefix = format!("{crate_id_path}/item/");
    let batch: Vec<BatchItem> = indexed_items(&index_crate)
        .into_iter()
        .map(|item| BatchItem {
            path: format!("{item_prefix}{}", item.fq_path),
            content: item.docs,
            title: item.name,
            doc_type: "rust-item".to_string(),
            tags: vec![crate_name.to_string(), "rust".to_string(), item.kind],
            source: "docsrs".to_string(),
        })
        .collect();

    Ok(store.upsert_batch_reconciled(&item_prefix, &batch)?)
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

    #[test]
    fn errors_when_root_id_is_missing_from_index() {
        // root points at "0:0", but that key doesn't exist in "index" at all
        // (malformed/unexpected response shape) -- must not be silently
        // treated the same as a legitimate "no docs" (docs: null) response,
        // or a malformed fetch could overwrite a good cached docstring with
        // an empty placeholder.
        let fixture = br#"{
            "root": "0:0",
            "index": { "1:1": { "docs": "some other item" } }
        }"#;
        let result = extract_root_docstring(fixture);
        assert!(result.is_err());
    }

    #[test]
    fn errors_when_docs_field_has_unexpected_type() {
        let fixture = br#"{
            "root": "0:0",
            "index": { "0:0": { "docs": 42 } }
        }"#;
        let result = extract_root_docstring(fixture);
        assert!(result.is_err());
    }

    #[test]
    fn extracts_none_when_docs_field_is_absent() {
        // rustdoc JSON commonly omits optional fields entirely (serde
        // skip_serializing_if) rather than emitting `null` -- an absent
        // "docs" key means the same thing as an explicit null: no docs.
        let fixture = br#"{
            "root": "0:0",
            "index": { "0:0": {} }
        }"#;
        let docs = extract_root_docstring(fixture).unwrap();
        assert_eq!(docs, None);
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
    fn fetch_and_store_indexes_individual_items_with_docs() {
        let raw_json = br#"{
            "root": 0,
            "index": {
                "0": { "docs": "A fake crate for testing." },
                "1": { "name": "State", "docs": "Extractor for shared state." },
                "2": { "name": "Router", "docs": "The router type." }
            },
            "paths": {
                "1": { "crate_id": 0, "path": ["fake_crate", "extract", "State"], "kind": "struct" },
                "2": { "crate_id": 0, "path": ["fake_crate", "Router"], "kind": "struct" }
            }
        }"#;
        let compressed = zstd::stream::encode_all(&raw_json[..], 0).unwrap();
        let fetcher = FakeFetcher {
            response: compressed,
        };
        let store = DocsStore::open_memory().unwrap();

        let outcome = fetch_and_store(&fetcher, &store, "fake-crate", "1.0.0").unwrap();
        assert_eq!(outcome.doc.content, "A fake crate for testing.");
        assert_eq!(outcome.items_indexed, 2, "both items should be written");
        assert!(outcome.items_error.is_none());

        let state_doc = store
            .get_by_path("docsrs/fake-crate/1.0.0/item/fake_crate::extract::State")
            .unwrap()
            .unwrap();
        assert_eq!(state_doc.content, "Extractor for shared state.");
        assert_eq!(state_doc.title, "State");

        let router_doc = store
            .get_by_path("docsrs/fake-crate/1.0.0/item/fake_crate::Router")
            .unwrap()
            .unwrap();
        assert_eq!(router_doc.content, "The router type.");

        let hits = store.search("Extractor for shared state", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].path,
            "docsrs/fake-crate/1.0.0/item/fake_crate::extract::State"
        );
    }

    #[test]
    fn refetch_removes_items_no_longer_present_in_the_crate_index() {
        // Regression for the staleness gap: item A and B are indexed by the
        // first fetch, then a refetch of the same crate_id_path ("latest")
        // only has item A -- B must no longer be gettable or searchable
        // afterward, not linger from the first fetch forever.
        let first_json = br#"{
            "root": 0,
            "index": {
                "0": { "docs": "A fake crate for testing." },
                "1": { "name": "State", "docs": "Extractor for shared state." },
                "2": { "name": "Router", "docs": "The router type." }
            },
            "paths": {
                "1": { "crate_id": 0, "path": ["fake_crate", "extract", "State"], "kind": "struct" },
                "2": { "crate_id": 0, "path": ["fake_crate", "Router"], "kind": "struct" }
            }
        }"#;
        let store = DocsStore::open_memory().unwrap();
        let fetcher = FakeFetcher {
            response: zstd::stream::encode_all(&first_json[..], 0).unwrap(),
        };
        fetch_and_store(&fetcher, &store, "fake-crate", "latest").unwrap();
        assert!(
            store
                .get_by_path("docsrs/fake-crate/latest/item/fake_crate::Router")
                .unwrap()
                .is_some()
        );

        // "latest" moved to a new version where Router was removed.
        let second_json = br#"{
            "root": 0,
            "index": {
                "0": { "docs": "A fake crate for testing." },
                "1": { "name": "State", "docs": "Extractor for shared state." }
            },
            "paths": {
                "1": { "crate_id": 0, "path": ["fake_crate", "extract", "State"], "kind": "struct" }
            }
        }"#;
        let fetcher = FakeFetcher {
            response: zstd::stream::encode_all(&second_json[..], 0).unwrap(),
        };
        fetch_and_store(&fetcher, &store, "fake-crate", "latest").unwrap();

        assert!(
            store
                .get_by_path("docsrs/fake-crate/latest/item/fake_crate::Router")
                .unwrap()
                .is_none(),
            "removed item must not be gettable by path after refetch"
        );
        let hits = store.search("The router type", 10).unwrap();
        assert!(
            hits.is_empty(),
            "removed item must not be findable via search after refetch"
        );

        // The item still present in the new fetch must be unaffected.
        assert!(
            store
                .get_by_path("docsrs/fake-crate/latest/item/fake_crate::extract::State")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn fetch_and_store_survives_unparseable_item_index() {
        // Regression: an item whose "kind" is a value the pinned
        // rustdoc-types version doesn't recognize fails IndexCrate's
        // deserialization for the whole crate -- fetch_and_store must still
        // return Ok with the overview doc, not propagate that as a failure.
        // But it must surface the failure in `items_error`, not just eat it.
        let raw_json = br#"{
            "root": 0,
            "index": {
                "0": { "docs": "A fake crate for testing." },
                "1": { "name": "Weird", "docs": "docs for an item rustdoc-types can't parse yet." }
            },
            "paths": {
                "1": { "crate_id": 0, "path": ["fake_crate", "Weird"], "kind": "some_future_kind" }
            }
        }"#;
        let compressed = zstd::stream::encode_all(&raw_json[..], 0).unwrap();
        let fetcher = FakeFetcher {
            response: compressed,
        };
        let store = DocsStore::open_memory().unwrap();

        let outcome = fetch_and_store(&fetcher, &store, "fake-crate", "1.0.0").unwrap();
        assert_eq!(outcome.doc.content, "A fake crate for testing.");
        assert_eq!(outcome.items_indexed, 0);
        assert!(
            outcome.items_error.is_some(),
            "the parse failure must be visible to the caller, not just eprintln'd"
        );

        // The unparseable item simply wasn't indexed -- no crash, no error.
        assert!(
            store
                .get_by_path("docsrs/fake-crate/1.0.0/item/fake_crate::Weird")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn fetch_and_store_persists_the_docstring() {
        let raw_json = br#"{
            "root": 0,
            "index": { "0": { "docs": "A fake crate for testing." } },
            "paths": {}
        }"#;
        let compressed = zstd::stream::encode_all(&raw_json[..], 0).unwrap();
        let fetcher = FakeFetcher {
            response: compressed,
        };
        let store = DocsStore::open_memory().unwrap();

        let outcome = fetch_and_store(&fetcher, &store, "fake-crate", "1.0.0").unwrap();

        assert_eq!(outcome.doc.content, "A fake crate for testing.");
        assert_eq!(outcome.doc.path, "docsrs/fake-crate/1.0.0");
        assert_eq!(outcome.doc.doc_type, "rust-crate");
        assert!(outcome.doc.blob_hash.is_some());
        assert_eq!(outcome.items_indexed, 0, "no items in this fixture");
        assert!(outcome.items_error.is_none());

        let hits = store.search("fake crate", 10).unwrap();
        assert_eq!(hits.len(), 1);
    }
}
