//! npm (and, by extension, JSR) ecosystem support: fetch a package, extract its
//! public API surface from TypeScript declaration files, and index it into the
//! same store the docs.rs path writes to.
//!
//! The public shape mirrors [`crate::rustdoc`] — a network-only
//! [`fetch_package`] and a local-only [`store_package`] — so the MCP server can
//! keep running the fetch on a blocking thread without ever holding the store
//! lock across an `.await`.

pub mod extract;
pub mod fetch;

use crate::FetchOutcome;
use crate::fetch::{ClientError, FetchError, Fetcher};
use crate::readme;
use crate::store::{BatchItem, DocsStore, Error as StoreError};
use agentflare_store::documents::DocUpsertOpts;
pub use extract::{ApiItem, ExtractError, extract, relative_imports};
pub use fetch::{DtsFile, MarkdownFile, NpmFetchError, PackageManifest, types_package_name};

#[derive(Debug, thiserror::Error)]
pub enum NpmError {
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error(transparent)]
    Npm(#[from] NpmFetchError),
    #[error(transparent)]
    Extract(#[from] ExtractError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl ClientError for NpmError {
    fn is_client_error(&self) -> bool {
        match self {
            NpmError::Fetch(e) => e.is_client_error(),
            // The package resolved fine; it just ships no types and has no
            // @types counterpart. That is a fact about what the caller asked
            // for, not a registry failure.
            NpmError::Npm(NpmFetchError::NoTypes(_)) => true,
            // A malformed manifest or unreadable tarball is the registry
            // serving something broken, and an extract/store failure is ours.
            _ => false,
        }
    }

    fn is_package_missing(&self) -> bool {
        match self {
            NpmError::Fetch(e) => e.is_package_missing(),
            // Deliberately not `NoTypes`: that package was found, it just
            // ships no declarations. Calling it missing would contradict the
            // error's own message.
            _ => false,
        }
    }
}

/// The [`DocsStore`] path an npm package's docs are cached under. Distinct
/// from the `docsrs/` prefix so both ecosystems coexist in one store and one
/// search index.
pub fn docs_id_path(package: &str, version: &str) -> String {
    format!("npm/{package}/{version}")
}

/// A usage example recovered from one of the package's markdown files.
#[derive(Debug, Clone)]
pub struct DocExample {
    /// The markdown file it came from, e.g. `README.md` or `docs/quickstart.md`.
    pub source_path: String,
    pub example: readme::ReadmeExample,
}

/// Everything the network step produced, ready for local indexing.
#[derive(Debug, Clone)]
pub struct FetchedPackage {
    /// Name the caller asked for (not the @types package, if one was used).
    pub package: String,
    /// Version the caller asked for, so cache paths stay stable for `latest`.
    pub requested_version: String,
    pub manifest: PackageManifest,
    pub dts: Vec<DtsFile>,
    /// Set when declarations came from DefinitelyTyped rather than the package.
    pub types_from: Option<String>,
    /// Usage examples pulled from the package's own README/guides — verbatim
    /// code the maintainer wrote, never LLM-generated.
    pub examples: Vec<DocExample>,
}

/// Resolves a package to its declaration files, following the DefinitelyTyped
/// fallback when the package ships none of its own.
///
/// Network only — no store access, no locks held.
pub fn fetch_package(
    fetcher: &dyn Fetcher,
    package: &str,
    version: &str,
) -> Result<FetchedPackage, NpmError> {
    let manifest_bytes = fetcher.fetch(&fetch::manifest_url(package, version))?;
    let manifest = fetch::parse_manifest(&manifest_bytes.bytes)?;

    let (source_manifest, types_from) = if manifest.has_types {
        (manifest.clone(), None)
    } else {
        // express/react and friends ship no types; DefinitelyTyped carries
        // them. Verified live: `npm:express` yields zero symbols while
        // `npm:@types/express` yields a full API surface.
        let types_pkg = types_package_name(package);
        let fetched = fetcher
            .fetch(&fetch::manifest_url(&types_pkg, "latest"))
            .map_err(|e| match e {
                // Only a 404 actually means "DefinitelyTyped has no package
                // for this". A timeout, DNS failure, or rate limit must
                // surface as itself -- reporting a retryable blip as a
                // permanent absence of types sends the caller to diagnose the
                // wrong thing.
                FetchError::Status(404) => {
                    NpmError::Npm(NpmFetchError::NoTypes(package.to_string()))
                }
                other => NpmError::Fetch(other),
            })?;
        let types_manifest = fetch::parse_manifest(&fetched.bytes)?;
        (types_manifest, Some(types_pkg))
    };

    let url = source_manifest
        .tarball_url
        .clone()
        .unwrap_or_else(|| fetch::tarball_url(&source_manifest.name, &source_manifest.version));
    let tarball = fetcher.fetch(&url)?;
    let dts = fetch::extract_dts(&tarball.bytes)?;
    // Same already-downloaded tarball, no extra fetch -- the README and any
    // bundled guides are just more files in it.
    let markdown = fetch::extract_markdown(&tarball.bytes)?;
    let examples = markdown
        .iter()
        .flat_map(|file| {
            readme::extract_readme_examples(&file.source)
                .into_iter()
                .map(move |example| DocExample {
                    source_path: file.path.clone(),
                    example,
                })
        })
        .collect();

    Ok(FetchedPackage {
        package: package.to_string(),
        requested_version: version.to_string(),
        manifest,
        dts,
        types_from,
        examples,
    })
}

/// Indexes an already-fetched package: one overview document plus one document
/// per exported API item. Local work only.
pub fn store_package(
    store: &DocsStore,
    fetched: &FetchedPackage,
) -> Result<FetchOutcome, NpmError> {
    let id_path = docs_id_path(&fetched.package, &fetched.requested_version);

    let mut overview = if fetched.manifest.description.is_empty() {
        format!("({} — no package description)", fetched.package)
    } else {
        fetched.manifest.description.clone()
    };
    overview.push_str(&format!("\n\nversion: {}", fetched.manifest.version));
    if let Some(homepage) = &fetched.manifest.homepage {
        overview.push_str(&format!("\nhomepage: {homepage}"));
    }
    if let Some(types_pkg) = &fetched.types_from {
        // Surfaced deliberately: the API below describes a community-maintained
        // type definition, which can lag the package it documents.
        overview.push_str(&format!("\ntypes from: {types_pkg} (DefinitelyTyped)"));
    }

    let mut tags = vec![
        fetched.package.clone(),
        "npm".to_string(),
        "node".to_string(),
    ];
    if fetched.types_from.is_some() {
        tags.push("definitelytyped".to_string());
    }

    let opts = DocUpsertOpts {
        title: Some(fetched.package.clone()),
        doc_type: Some("npm-package".to_string()),
        source: Some("npm".to_string()),
        tags: Some(tags),
        track_history: false,
        ..Default::default()
    };
    let overview_doc = store.upsert(&id_path, &overview, opts)?;

    // Per-item indexing is best-effort on top of an overview that already
    // succeeded — a parse failure in one package must not report the whole
    // fetch as failed, matching the rustdoc path's behaviour. The failure is
    // reported through the outcome rather than only to stderr, so callers can
    // actually observe it.
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

/// Fetch and index in one call. Prefer the split form when the fetch must run
/// off the calling thread.
pub fn fetch_and_store(
    fetcher: &dyn Fetcher,
    store: &DocsStore,
    package: &str,
    version: &str,
) -> Result<FetchOutcome, NpmError> {
    let fetched = fetch_package(fetcher, package, version)?;
    store_package(store, &fetched)
}

fn index_items(
    store: &DocsStore,
    fetched: &FetchedPackage,
    crate_id_path: &str,
) -> Result<usize, NpmError> {
    let item_prefix = format!("{crate_id_path}/item/");
    let mut batch: Vec<BatchItem> = Vec::new();
    // Declaration files re-export heavily, so the same symbol shows up in an
    // entry point and its defining module. Keep the first occurrence — files
    // are sorted, so the choice is stable across refetches and the reconcile
    // pass below can't thrash.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for file in &fetched.dts {
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
                doc_type: "npm-item".to_string(),
                tags: vec![
                    fetched.package.clone(),
                    "npm".to_string(),
                    "node".to_string(),
                    item.kind,
                ],
                source: "npm".to_string(),
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
) -> Result<usize, NpmError> {
    let example_prefix = format!("{package_id_path}/example/");
    let batch: Vec<BatchItem> = fetched
        .examples
        .iter()
        .enumerate()
        .map(|(i, doc_example)| {
            let title = if doc_example.source_path == "README.md" {
                doc_example.example.title.clone()
            } else {
                format!("{}: {}", doc_example.source_path, doc_example.example.title)
            };
            BatchItem {
                path: format!(
                    "{example_prefix}{i:03}-{}",
                    slugify(&doc_example.example.title)
                ),
                content: doc_example.example.code.clone(),
                title,
                doc_type: "npm-example".to_string(),
                tags: vec![
                    fetched.package.clone(),
                    "npm".to_string(),
                    "node".to_string(),
                    "example".to_string(),
                ],
                source: "npm".to_string(),
            }
        })
        .collect();
    Ok(store.upsert_batch_reconciled(&example_prefix, &batch)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::FetchedBytes;

    /// Serves canned responses keyed by URL substring, so the fallback path can
    /// be driven without network access.
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
            // An unregistered URL models "the registry has no such package",
            // i.e. a 404 -- not a transport failure.
            Err(FetchError::Status(404))
        }
    }

    /// Serves the source manifest but fails the `@types` lookup with a
    /// transport error, to distinguish "no types published" from "the network
    /// misbehaved".
    struct FlakyTypesFetcher {
        source: Vec<u8>,
    }

    impl Fetcher for FlakyTypesFetcher {
        fn fetch(&self, url: &str) -> Result<FetchedBytes, FetchError> {
            if url.contains("@types/") {
                return Err(FetchError::Http("timed out".into()));
            }
            Ok(FetchedBytes {
                bytes: self.source.clone(),
                etag: None,
                content_type: None,
            })
        }
    }

    fn tarball(files: &[(&str, &str)]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            for (path, body) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(body.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, format!("package/{path}"), body.as_bytes())
                    .unwrap();
            }
            builder.finish().unwrap();
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar_bytes).unwrap();
        gz.finish().unwrap()
    }

    const HONO_DTS: &str = r#"
/**
 * The request context.
 */
export declare class Context {
    /**
     * `.json()` renders JSON as `Content-Type:application/json`.
     */
    json(object: unknown, status?: number): Response;
}
"#;

    #[test]
    fn indexes_package_overview_and_per_item_docs() {
        let fetcher = FakeFetcher {
            routes: vec![
                (
                    "registry.npmjs.org/hono/4.6.3".to_string(),
                    br#"{"name":"hono","version":"4.6.3","description":"Web framework",
                         "types":"./dist/types/index.d.ts","homepage":"https://hono.dev"}"#
                        .to_vec(),
                ),
                (
                    "-/hono-4.6.3.tgz".to_string(),
                    tarball(&[("dist/types/context.d.ts", HONO_DTS)]),
                ),
            ],
        };
        let store = DocsStore::open_memory().unwrap();
        let outcome = fetch_and_store(&fetcher, &store, "hono", "4.6.3").unwrap();
        assert!(outcome.items_indexed > 0, "{outcome:?}");
        assert_eq!(outcome.items_error, None);
        let doc = outcome.doc;

        assert_eq!(doc.path, "npm/hono/4.6.3");
        assert_eq!(doc.doc_type, "npm-package");
        assert!(doc.content.contains("Web framework"));
        assert!(doc.content.contains("https://hono.dev"));

        let member = store
            .get_by_path("npm/hono/4.6.3/item/Context.json")
            .unwrap()
            .expect("class members must be indexed individually");
        assert_eq!(member.title, "json");
        assert!(
            member
                .content
                .contains("json(object: unknown, status?: number): Response")
        );
        assert!(member.content.contains("renders JSON"));

        // The signature must be reachable by search, not just by exact path —
        // that is the whole point of indexing per item.
        let hits = store.search("renders JSON", 10).unwrap();
        assert!(
            hits.iter().any(|h| h.path.ends_with("Context.json")),
            "{hits:?}"
        );
    }

    #[test]
    fn a_failed_types_lookup_reports_the_transport_error_not_missing_types() {
        let fetcher = FlakyTypesFetcher {
            source: br#"{"name":"express","version":"4.18.2","description":"Fast web framework"}"#
                .to_vec(),
        };
        let err = fetch_package(&fetcher, "express", "4.18.2").unwrap_err();
        assert!(
            matches!(err, NpmError::Fetch(FetchError::Http(_))),
            "a timeout on the @types lookup must surface as itself, not as \
             \"ships no types\" -- otherwise a retryable blip reads as a \
             permanent absence: {err:?}"
        );
    }

    #[test]
    fn a_package_with_no_types_and_no_definitelytyped_entry_reports_no_types() {
        let fetcher = FakeFetcher {
            routes: vec![(
                "registry.npmjs.org/obscure-pkg/1.0.0".to_string(),
                br#"{"name":"obscure-pkg","version":"1.0.0","description":"no types anywhere"}"#
                    .to_vec(),
            )],
        };
        let err = fetch_package(&fetcher, "obscure-pkg", "1.0.0").unwrap_err();
        assert!(
            matches!(err, NpmError::Npm(NpmFetchError::NoTypes(_))),
            "a 404 on the @types lookup is the genuine no-types signal: {err:?}"
        );
    }

    #[test]
    fn falls_back_to_definitelytyped_when_a_package_ships_no_types() {
        // Verified live: express 4.18.2 yields zero symbols on its own, while
        // @types/express carries the full surface.
        let fetcher = FakeFetcher {
            routes: vec![
                (
                    "registry.npmjs.org/express/4.18.2".to_string(),
                    br#"{"name":"express","version":"4.18.2","description":"Fast web framework"}"#
                        .to_vec(),
                ),
                (
                    "registry.npmjs.org/@types/express/latest".to_string(),
                    br#"{"name":"@types/express","version":"4.17.21","description":"TypeScript definitions for express"}"#
                        .to_vec(),
                ),
                (
                    "-/express-4.17.21.tgz".to_string(),
                    tarball(&[(
                        "index.d.ts",
                        "export declare function Router(): void;\n",
                    )]),
                ),
            ],
        };
        let store = DocsStore::open_memory().unwrap();
        let doc = fetch_and_store(&fetcher, &store, "express", "4.18.2")
            .unwrap()
            .doc;

        // Cached under the name the caller asked for, not the @types name.
        assert_eq!(doc.path, "npm/express/4.18.2");
        // Provenance is surfaced: DefinitelyTyped can lag the real package.
        assert!(doc.content.contains("@types/express"), "{}", doc.content);
        assert!(
            store
                .get_by_path("npm/express/4.18.2/item/Router")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn refetch_drops_items_that_left_the_api() {
        let store = DocsStore::open_memory().unwrap();
        let manifest = br#"{"name":"p","version":"1.0.0","description":"d","types":"./i.d.ts"}"#;
        let first = FakeFetcher {
            routes: vec![
                ("registry.npmjs.org/p/latest".to_string(), manifest.to_vec()),
                (
                    ".tgz".to_string(),
                    tarball(&[(
                        "i.d.ts",
                        "export declare function keep(): void;\nexport declare function drop(): void;\n",
                    )]),
                ),
            ],
        };
        fetch_and_store(&first, &store, "p", "latest").unwrap();
        assert!(
            store
                .get_by_path("npm/p/latest/item/drop")
                .unwrap()
                .is_some()
        );

        let second = FakeFetcher {
            routes: vec![
                ("registry.npmjs.org/p/latest".to_string(), manifest.to_vec()),
                (
                    ".tgz".to_string(),
                    tarball(&[("i.d.ts", "export declare function keep(): void;\n")]),
                ),
            ],
        };
        fetch_and_store(&second, &store, "p", "latest").unwrap();

        assert!(
            store
                .get_by_path("npm/p/latest/item/drop")
                .unwrap()
                .is_none(),
            "a removed export must stop being gettable"
        );
        assert!(
            store
                .get_by_path("npm/p/latest/item/keep")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn a_package_without_declaration_files_still_stores_an_overview() {
        let fetcher = FakeFetcher {
            routes: vec![
                (
                    "registry.npmjs.org/plain/1.0.0".to_string(),
                    br#"{"name":"plain","version":"1.0.0","description":"no types here","types":"./x.d.ts"}"#.to_vec(),
                ),
                (".tgz".to_string(), tarball(&[("index.js", "module.exports={}")])),
            ],
        };
        let store = DocsStore::open_memory().unwrap();
        let outcome = fetch_and_store(&fetcher, &store, "plain", "1.0.0").unwrap();
        assert!(outcome.doc.content.contains("no types here"));
        assert_eq!(outcome.items_indexed, 0);
    }

    #[test]
    fn ecosystem_paths_do_not_collide_with_the_rustdoc_prefix() {
        assert_eq!(docs_id_path("hono", "4.6.3"), "npm/hono/4.6.3");
        assert_eq!(
            crate::docs_id_path("serde", "latest"),
            "docsrs/serde/latest"
        );
    }
}
