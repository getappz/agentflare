//! Python (PyPI) ecosystem support: fetch a package, extract its public API
//! surface from PEP 561 `.pyi` stub files, and index it into the same store
//! the docs.rs and npm paths write to.
//!
//! The public shape mirrors [`crate::npm`] — a network-only [`fetch_package`]
//! and a local-only [`store_package`] — so the MCP server can keep running
//! the fetch on a blocking thread without ever holding the store lock across
//! an `.await`.

pub mod extract;
pub mod fetch;

use crate::FetchOutcome;
use crate::fetch::{ClientError, FetchError, Fetcher};
use crate::store::{BatchItem, DocsStore, Error as StoreError};
use agentflare_store::documents::DocUpsertOpts;
pub use extract::{ApiItem, ExtractError};
pub use fetch::{PackageManifest, PyiFile, PythonFetchError, types_package_name};
use crate::readme;

#[derive(Debug, thiserror::Error)]
pub enum PythonError {
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error(transparent)]
    Python(#[from] PythonFetchError),
    #[error(transparent)]
    Extract(#[from] ExtractError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl ClientError for PythonError {
    fn is_client_error(&self) -> bool {
        match self {
            PythonError::Fetch(e) => e.is_client_error(),
            // The package resolved fine; it just ships no stubs and has no
            // types-<package> counterpart. That is a fact about what the
            // caller asked for, not a registry failure.
            PythonError::Python(PythonFetchError::NoTypes(_)) => true,
            // A malformed manifest or unreadable wheel is the registry
            // serving something broken, and an extract/store failure is ours.
            _ => false,
        }
    }

    fn is_package_missing(&self) -> bool {
        match self {
            PythonError::Fetch(e) => e.is_package_missing(),
            // Deliberately not `NoTypes`: that package was found, it just
            // ships no stubs. Calling it missing would contradict the
            // error's own message.
            _ => false,
        }
    }
}

/// The [`DocsStore`] path a Python package's docs are cached under. Distinct
/// from the `docsrs/`/`npm/` prefixes so all three ecosystems coexist in one
/// store and one search index.
pub fn docs_id_path(package: &str, version: &str) -> String {
    format!("pypi/{package}/{version}")
}

/// Everything the network step produced, ready for local indexing.
#[derive(Debug, Clone)]
pub struct FetchedPackage {
    /// Name the caller asked for (not the types-<package> package, if one
    /// was used).
    pub package: String,
    /// Version the caller asked for, so cache paths stay stable for `latest`.
    pub requested_version: String,
    pub manifest: PackageManifest,
    pub pyi: Vec<PyiFile>,
    /// Set when stubs came from typeshed rather than the package itself.
    pub types_from: Option<String>,
    /// Usage examples pulled from the package's own PyPI long description
    /// (markdown releases only) — verbatim code the maintainer wrote, never
    /// LLM-generated.
    pub examples: Vec<readme::ReadmeExample>,
}

/// Resolves a package to its stub files, following the typeshed fallback
/// when the package ships none of its own (or ships a wheel with no `.pyi`
/// files in it — e.g. a C-extension package).
///
/// Network only — no store access, no locks held.
pub fn fetch_package(
    fetcher: &dyn Fetcher,
    package: &str,
    version: &str,
) -> Result<FetchedPackage, PythonError> {
    let manifest_bytes = fetcher.fetch(&fetch::manifest_url(package, version))?;
    let manifest = fetch::parse_manifest(&manifest_bytes.bytes)?;

    let (pyi, types_from) = match &manifest.wheel_url {
        Some(url) => {
            let wheel = fetcher.fetch(url)?;
            let pyi = fetch::extract_pyi(&wheel.bytes)?;
            if pyi.is_empty() {
                fetch_typeshed_fallback(fetcher, package)?
            } else {
                (pyi, None)
            }
        }
        None => fetch_typeshed_fallback(fetcher, package)?,
    };

    let examples = manifest
        .readme
        .as_deref()
        .map(readme::extract_readme_examples)
        .unwrap_or_default();

    Ok(FetchedPackage {
        package: package.to_string(),
        requested_version: version.to_string(),
        manifest,
        pyi,
        types_from,
        examples,
    })
}

/// Looks up `types-<package>` on PyPI and unpacks its stubs. Verified live:
/// `pypi:requests` yields no stubs of its own for older releases lacking a
/// `py.typed` wheel, while `pypi:types-requests` carries a full API surface.
fn fetch_typeshed_fallback(
    fetcher: &dyn Fetcher,
    package: &str,
) -> Result<(Vec<PyiFile>, Option<String>), PythonError> {
    let types_pkg = fetch::types_package_name(package);
    let fetched = fetcher
        .fetch(&fetch::manifest_url(&types_pkg, "latest"))
        .map_err(|e| match e {
            // Only a 404 actually means "typeshed has no package for this".
            // A timeout, DNS failure, or rate limit must surface as itself --
            // reporting a retryable blip as a permanent absence of stubs
            // sends the caller to diagnose the wrong thing.
            FetchError::Status(404) => {
                PythonError::Python(PythonFetchError::NoTypes(package.to_string()))
            }
            other => PythonError::Fetch(other),
        })?;
    let types_manifest = fetch::parse_manifest(&fetched.bytes)?;
    let Some(wheel_url) = types_manifest.wheel_url else {
        return Err(PythonError::Python(PythonFetchError::NoTypes(
            package.to_string(),
        )));
    };
    let wheel = fetcher.fetch(&wheel_url)?;
    let pyi = fetch::extract_pyi(&wheel.bytes)?;
    if pyi.is_empty() {
        return Err(PythonError::Python(PythonFetchError::NoTypes(
            package.to_string(),
        )));
    }
    Ok((pyi, Some(types_pkg)))
}

/// Indexes an already-fetched package: one overview document plus one
/// document per public API item. Local work only.
pub fn store_package(
    store: &DocsStore,
    fetched: &FetchedPackage,
) -> Result<FetchOutcome, PythonError> {
    let id_path = docs_id_path(&fetched.package, &fetched.requested_version);

    let mut overview = if fetched.manifest.summary.is_empty() {
        format!("({} — no package summary)", fetched.package)
    } else {
        fetched.manifest.summary.clone()
    };
    overview.push_str(&format!("\n\nversion: {}", fetched.manifest.version));
    if let Some(homepage) = &fetched.manifest.homepage {
        overview.push_str(&format!("\nhomepage: {homepage}"));
    }
    if let Some(types_pkg) = &fetched.types_from {
        // Surfaced deliberately: the API below describes a community-
        // maintained type stub, which can lag the package it documents.
        overview.push_str(&format!("\ntypes from: {types_pkg} (typeshed)"));
    }

    let mut tags = vec![
        fetched.package.clone(),
        "python".to_string(),
        "pypi".to_string(),
    ];
    if fetched.types_from.is_some() {
        tags.push("typeshed".to_string());
    }

    let opts = DocUpsertOpts {
        title: Some(fetched.package.clone()),
        doc_type: Some("python-package".to_string()),
        source: Some("python".to_string()),
        tags: Some(tags),
        track_history: false,
        ..Default::default()
    };
    let overview_doc = store.upsert(&id_path, &overview, opts)?;

    // Per-item indexing is best-effort on top of an overview that already
    // succeeded -- a parse failure in one file must not report the whole
    // fetch as failed, matching the rustdoc and npm paths' behaviour.
    let (items_indexed, items_error) = match index_items(store, fetched, &id_path) {
        Ok(n) => (n, None),
        Err(e) => (0, Some(e.to_string())),
    };
    let (examples_indexed, examples_error) = match index_examples(store, fetched, &id_path) {
        Ok(n) => (n, None),
        Err(e) => (0, Some(e.to_string())),
    };
    Ok(FetchOutcome {
        doc: overview_doc,
        items_indexed: items_indexed + examples_indexed,
        items_error: match (items_error, examples_error) {
            (Some(a), Some(b)) => Some(format!("{a}; {b}")),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        },
    })
}

/// Fetch and index in one call. Prefer the split form when the fetch must
/// run off the calling thread.
pub fn fetch_and_store(
    fetcher: &dyn Fetcher,
    store: &DocsStore,
    package: &str,
    version: &str,
) -> Result<FetchOutcome, PythonError> {
    let fetched = fetch_package(fetcher, package, version)?;
    store_package(store, &fetched)
}

fn index_items(
    store: &DocsStore,
    fetched: &FetchedPackage,
    package_id_path: &str,
) -> Result<usize, PythonError> {
    let item_prefix = format!("{package_id_path}/item/");
    let mut batch: Vec<BatchItem> = Vec::new();
    // Stub packages re-export heavily (a package's `__init__.pyi` commonly
    // re-imports names its submodules define), so the same symbol can show
    // up in more than one file. Keep the first occurrence -- files are
    // sorted, so the choice is stable across refetches.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for file in &fetched.pyi {
        let items = match extract::extract(&file.source) {
            Ok(items) => items,
            // One unparseable file must not lose the rest of the package.
            Err(_) => continue,
        };
        for item in items {
            if !seen.insert(item.fq_path.clone()) {
                continue;
            }
            batch.push(BatchItem {
                path: format!("{item_prefix}{}", item.fq_path),
                content: item.content(),
                title: item.name,
                doc_type: "python-item".to_string(),
                tags: vec![
                    fetched.package.clone(),
                    "python".to_string(),
                    "pypi".to_string(),
                    item.kind,
                ],
                source: "python".to_string(),
            });
        }
    }
    Ok(store.upsert_batch_reconciled(&item_prefix, &batch)?)
}

/// Lowercase, hyphen-joined form of a title, for a stable, readable path
/// segment ("Quick Start!" -> "quick-start").
fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "example".to_string()
    } else {
        out
    }
}

fn index_examples(
    store: &DocsStore,
    fetched: &FetchedPackage,
    package_id_path: &str,
) -> Result<usize, PythonError> {
    let example_prefix = format!("{package_id_path}/example/");
    let batch: Vec<BatchItem> = fetched
        .examples
        .iter()
        .enumerate()
        .map(|(i, example)| BatchItem {
            path: format!("{example_prefix}{i:03}-{}", slugify(&example.title)),
            content: example.code.clone(),
            title: example.title.clone(),
            doc_type: "python-example".to_string(),
            tags: vec![
                fetched.package.clone(),
                "python".to_string(),
                "pypi".to_string(),
                "example".to_string(),
            ],
            source: "python".to_string(),
        })
        .collect();
    Ok(store.upsert_batch_reconciled(&example_prefix, &batch)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::FetchedBytes;

    /// Serves canned responses keyed by URL substring, so the fallback path
    /// can be driven without network access.
    struct FakeFetcher {
        routes: Vec<(String, Vec<u8>)>,
    }

    impl Fetcher for FakeFetcher {
        fn fetch(&self, url: &str) -> Result<FetchedBytes, FetchError> {
            for (needle, body) in &self.routes {
                if url.contains(needle.as_str()) {
                    return Ok(FetchedBytes {
                        bytes: body.clone(),
                        etag: None,
                        content_type: None,
                    });
                }
            }
            // An unregistered URL models "the registry has no such
            // package/release", i.e. a 404 -- not a transport failure.
            Err(FetchError::Status(404))
        }
    }

    /// Serves the source manifest but fails the typeshed lookup with a
    /// transport error, to distinguish "no stubs published" from "the
    /// network misbehaved".
    struct FlakyTypeshedFetcher {
        source: Vec<u8>,
    }

    impl Fetcher for FlakyTypeshedFetcher {
        fn fetch(&self, url: &str) -> Result<FetchedBytes, FetchError> {
            if url.contains("types-") {
                return Err(FetchError::Http("timed out".into()));
            }
            Ok(FetchedBytes {
                bytes: self.source.clone(),
                etag: None,
                content_type: None,
            })
        }
    }

    fn fake_wheel(files: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut buf);
            let mut writer = zip::ZipWriter::new(cursor);
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (path, body) in files {
                writer.start_file(*path, opts).unwrap();
                std::io::Write::write_all(&mut writer, body.as_bytes()).unwrap();
            }
            writer.finish().unwrap();
        }
        buf
    }

    const REQUESTS_PYI: &str = r#"
class Session:
    """A persistent HTTP session."""
    def request(self, method: str, url: str) -> Response:
        """Sends a request and returns its response."""
        ...
"#;

    fn manifest_json(name: &str, version: &str, summary: &str, wheel_url: &str) -> Vec<u8> {
        format!(
            r#"{{"info": {{"name": "{name}", "version": "{version}", "summary": "{summary}",
                 "home_page": "https://example.com/{name}"}},
               "urls": [{{"packagetype": "bdist_wheel",
                          "filename": "{name}-{version}-py3-none-any.whl",
                          "url": "{wheel_url}"}}]}}"#
        )
        .into_bytes()
    }

    #[test]
    fn indexes_package_overview_and_per_item_docs() {
        let fetcher = FakeFetcher {
            routes: vec![
                (
                    "pypi.org/pypi/requests/2.32.3".to_string(),
                    manifest_json(
                        "requests",
                        "2.32.3",
                        "Python HTTP for Humans.",
                        "https://files/requests-2.32.3-py3-none-any.whl",
                    ),
                ),
                (
                    "requests-2.32.3-py3-none-any.whl".to_string(),
                    fake_wheel(&[("requests/api.pyi", REQUESTS_PYI)]),
                ),
            ],
        };
        let store = DocsStore::open_memory().unwrap();
        let outcome = fetch_and_store(&fetcher, &store, "requests", "2.32.3").unwrap();
        assert!(outcome.items_indexed > 0, "{outcome:?}");
        assert_eq!(outcome.items_error, None);
        let doc = outcome.doc;

        assert_eq!(doc.path, "pypi/requests/2.32.3");
        assert_eq!(doc.doc_type, "python-package");
        assert!(doc.content.contains("Python HTTP for Humans."));
        assert!(doc.content.contains("https://example.com/requests"));

        let member = store
            .get_by_path("pypi/requests/2.32.3/item/Session.request")
            .unwrap()
            .expect("class members must be indexed individually");
        assert_eq!(member.title, "request");
        assert!(
            member
                .content
                .contains("def request(self, method: str, url: str) -> Response")
        );
        assert!(member.content.contains("Sends a request"));

        let hits = store.search("Sends a request", 10).unwrap();
        assert!(
            hits.iter().any(|h| h.path.ends_with("Session.request")),
            "{hits:?}"
        );
    }

    #[test]
    fn indexes_examples_from_a_markdown_readme() {
        let manifest = br##"{
            "info": {
                "name": "p", "version": "1.0.0", "summary": "d",
                "home_page": "https://example.com/p",
                "description": "# p\n\n## Usage\n\n```python\nimport p\n\nclient = p.Client()\nclient.run()\n```\n",
                "description_content_type": "text/markdown"
            },
            "urls": [{"packagetype": "bdist_wheel",
                       "filename": "p-1.0.0-py3-none-any.whl",
                       "url": "https://files/p-1.0.0-py3-none-any.whl"}]}"##;
        let fetcher = FakeFetcher {
            routes: vec![
                ("pypi.org/pypi/p/1.0.0".to_string(), manifest.to_vec()),
                (
                    "p-1.0.0-py3-none-any.whl".to_string(),
                    fake_wheel(&[("p.pyi", "def run() -> None: ...\n")]),
                ),
            ],
        };
        let store = DocsStore::open_memory().unwrap();
        let outcome = fetch_and_store(&fetcher, &store, "p", "1.0.0").unwrap();
        assert_eq!(outcome.items_error, None);

        let examples = store.list_summaries(None, 0).unwrap();
        let example = examples
            .iter()
            .find(|d| d.path.starts_with("pypi/p/1.0.0/example/"))
            .expect("a markdown example must be indexed");
        assert_eq!(example.doc_type, "python-example");

        let doc = store.get_by_path(&example.path).unwrap().unwrap();
        assert_eq!(doc.title, "Usage");
        assert!(doc.content.contains("client.run()"));

        let hits = store.search("client.run", 10).unwrap();
        assert!(
            hits.iter().any(|h| h.path == example.path),
            "example must be searchable: {hits:?}"
        );
    }

    #[test]
    fn a_failed_typeshed_lookup_reports_the_transport_error_not_missing_types() {
        // No wheel_url in the manifest -> straight to the typeshed fallback,
        // which then times out rather than 404ing.
        let manifest = br#"{"info": {"name": "obscure", "version": "1.0.0", "summary": "d"}, "urls": []}"#;
        let fetcher = FlakyTypeshedFetcher {
            source: manifest.to_vec(),
        };
        let err = fetch_package(&fetcher, "obscure", "1.0.0").unwrap_err();
        assert!(
            matches!(err, PythonError::Fetch(FetchError::Http(_))),
            "a timeout on the typeshed lookup must surface as itself, not as \
             \"ships no types\" -- otherwise a retryable blip reads as a \
             permanent absence: {err:?}"
        );
    }

    #[test]
    fn a_package_with_no_wheel_and_no_typeshed_entry_reports_no_types() {
        let fetcher = FakeFetcher {
            routes: vec![(
                "pypi.org/pypi/obscure-pkg/1.0.0".to_string(),
                br#"{"info": {"name": "obscure-pkg", "version": "1.0.0", "summary": "no wheels anywhere"}, "urls": []}"#.to_vec(),
            )],
        };
        let err = fetch_package(&fetcher, "obscure-pkg", "1.0.0").unwrap_err();
        assert!(
            matches!(err, PythonError::Python(PythonFetchError::NoTypes(_))),
            "a 404 on the typeshed lookup is the genuine no-types signal: {err:?}"
        );
    }

    #[test]
    fn falls_back_to_typeshed_when_the_packages_own_wheel_has_no_stubs() {
        // The shape a C-extension package with no bundled .pyi files takes:
        // it has a pure-Python wheel entry, but that wheel carries no stubs.
        let fetcher = FakeFetcher {
            routes: vec![
                (
                    "pypi.org/pypi/plain/1.0.0".to_string(),
                    manifest_json(
                        "plain",
                        "1.0.0",
                        "no stubs of its own",
                        "https://files/plain-1.0.0-py3-none-any.whl",
                    ),
                ),
                (
                    "plain-1.0.0-py3-none-any.whl".to_string(),
                    fake_wheel(&[("plain/mod.py", "def f(): pass")]),
                ),
                (
                    "pypi.org/pypi/types-plain/json".to_string(),
                    manifest_json(
                        "types-plain",
                        "1.0.0.20260101",
                        "Typing stubs for plain",
                        "https://files/types_plain-1.0.0.20260101-py3-none-any.whl",
                    ),
                ),
                (
                    "types_plain-1.0.0.20260101-py3-none-any.whl".to_string(),
                    fake_wheel(&[("plain-stubs/mod.pyi", "def f() -> None: ...\n")]),
                ),
            ],
        };
        let store = DocsStore::open_memory().unwrap();
        let doc = fetch_and_store(&fetcher, &store, "plain", "1.0.0")
            .unwrap()
            .doc;

        // Cached under the name the caller asked for, not the types- name.
        assert_eq!(doc.path, "pypi/plain/1.0.0");
        // Provenance is surfaced: typeshed can lag the real package.
        assert!(doc.content.contains("types-plain"), "{}", doc.content);
        assert!(
            store
                .get_by_path("pypi/plain/1.0.0/item/f")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn refetch_drops_items_that_left_the_api() {
        let store = DocsStore::open_memory().unwrap();
        let manifest = manifest_json("p", "1.0.0", "d", "https://files/p-1.0.0-py3-none-any.whl");
        let first = FakeFetcher {
            routes: vec![
                ("pypi.org/pypi/p/json".to_string(), manifest.clone()),
                (
                    "p-1.0.0-py3-none-any.whl".to_string(),
                    fake_wheel(&[(
                        "p.pyi",
                        "def keep() -> None: ...\ndef drop() -> None: ...\n",
                    )]),
                ),
            ],
        };
        fetch_and_store(&first, &store, "p", "latest").unwrap();
        assert!(store.get_by_path("pypi/p/latest/item/drop").unwrap().is_some());

        let second = FakeFetcher {
            routes: vec![
                ("pypi.org/pypi/p/json".to_string(), manifest),
                (
                    "p-1.0.0-py3-none-any.whl".to_string(),
                    fake_wheel(&[("p.pyi", "def keep() -> None: ...\n")]),
                ),
            ],
        };
        fetch_and_store(&second, &store, "p", "latest").unwrap();

        assert!(
            store.get_by_path("pypi/p/latest/item/drop").unwrap().is_none(),
            "a removed export must stop being gettable"
        );
        assert!(store.get_by_path("pypi/p/latest/item/keep").unwrap().is_some());
    }

    #[test]
    fn ecosystem_paths_do_not_collide_with_other_prefixes() {
        assert_eq!(docs_id_path("requests", "2.32.3"), "pypi/requests/2.32.3");
        assert_eq!(
            crate::docs_id_path("serde", "latest"),
            "docsrs/serde/latest"
        );
        assert_eq!(
            crate::npm::docs_id_path("hono", "4.6.3"),
            "npm/hono/4.6.3"
        );
    }
}
