//! `flare_docs` MCP tool handler body.

use super::*;
// `pub(crate)` (not a plain `use`): the local `mod flare_docs` declared in
// `mcp_server.rs` shadows the `flare_docs` extern crate for any unqualified
// `flare_docs::...` path written in that parent file (extern-prelude entries
// are shadowed by same-named local items). `mcp_server.rs`'s struct field and
// `ensure_flare_docs_store`/`with_flare_docs_store` helpers reference
// `flare_docs::DocsStore` expecting the crate type, so it must be
// re-exported (not just privately imported) through this submodule for that
// path to resolve.
pub(crate) use ::flare_docs::{
    ClientError, DocsStore, Ecosystem, FetchOutcome, Fetcher, GcOpts, UreqFetcher,
    docs_rs_json_url, npm, python, store_fetched,
};

const DEFAULT_LIMIT: usize = 10;
const DEFAULT_VERSION: &str = "latest";
/// Caps how long a single MCP tool call can block on a docs.rs fetch. Shorter
/// than `UreqFetcher`'s 300s read timeout so a stalled response fails fast
/// with a clear error instead of freezing the calling agent/session.
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

impl AgentflareMcp {
    pub async fn flare_docs_impl(&self, req: FlareDocsRequest) -> Result<String, ErrorData> {
        match req.action.as_str() {
            "search" => {
                let query = req
                    .query
                    .ok_or_else(|| ErrorData::invalid_params("search requires \"query\"", None))?;
                let limit = req.limit.unwrap_or(DEFAULT_LIMIT);
                self.with_flare_docs_store(|store| {
                    let hits = store
                        .search(&query, limit)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    serde_json::to_string(&hits)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
                })
                .await?
            }
            "list" => {
                // Bodies are deliberately omitted, and the page is capped
                // even when the caller names no limit. `list` used to return
                // every cached document in full; per-item indexing means one
                // crate contributes hundreds of documents, so a routine cache
                // reached 10,889 documents / 7.75 MB -- a single response no
                // context window can hold. `total` travels with the page so a
                // truncated listing can never be mistaken for a complete one.
                let offset = req.offset.unwrap_or(0);
                self.with_flare_docs_store(|store| {
                    let total = store
                        .count()
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    let docs = store
                        .list_summaries(req.limit, offset)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    serde_json::to_string(&serde_json::json!({
                        "total": total,
                        "offset": offset,
                        "returned": docs.len(),
                        "docs": docs,
                    }))
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))
                })
                .await?
            }
            "get" if req.id.is_some() => {
                let id = req.id.expect("guarded by is_some() above");
                self.with_flare_docs_store(|store| {
                    let doc = store
                        .get(&id)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    serde_json::to_string(&doc)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
                })
                .await?
            }
            "get" => {
                let package = req.package.ok_or_else(|| {
                    ErrorData::invalid_params("get requires \"id\" or \"package\"", None)
                })?;
                let eco = Ecosystem::resolve(req.ecosystem.as_deref(), &package)
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                let version = req.version.unwrap_or_else(|| DEFAULT_VERSION.to_string());
                let cached = self
                    .with_flare_docs_store(|store| {
                        store
                            .get_by_path(&eco.docs_id_path(&package, &version))
                            .map_err(|e| ErrorData::internal_error(e.to_string(), None))
                    })
                    .await??;
                match cached {
                    Some(doc) => serde_json::to_string(&doc)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None)),
                    // A cache-miss "get" is still just a document lookup --
                    // return only the doc, matching the cache-hit shape
                    // above, rather than inventing fetch-outcome telemetry a
                    // plain "get" never asked for ("refresh" is the action
                    // for that).
                    None => {
                        let outcome = self
                            .fetch_and_store_via_spawn_blocking(eco, package, version)
                            .await?;
                        serde_json::to_string(&outcome.doc)
                            .map_err(|e| ErrorData::internal_error(e.to_string(), None))
                    }
                }
            }
            "refresh" => {
                let package = req.package.ok_or_else(|| {
                    ErrorData::invalid_params("refresh requires \"package\"", None)
                })?;
                let eco = Ecosystem::resolve(req.ecosystem.as_deref(), &package)
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                let version = req.version.unwrap_or_else(|| DEFAULT_VERSION.to_string());
                let outcome = self
                    .fetch_and_store_via_spawn_blocking(eco, package, version)
                    .await?;
                serde_json::to_string(&outcome)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown action \"{other}\" (expected search|get|list|refresh)"),
                None,
            )),
        }
    }

    /// Runs the registry network fetch on tokio's blocking thread pool (never
    /// inline on the single-threaded MCP runtime, and never while holding the
    /// mutex guarding `flare_docs_store`), then, once the fetch has
    /// completed, does the fast local parse/store work through
    /// `with_flare_docs_store`.
    ///
    /// npm needs several sequential requests (version manifest, possibly a
    /// DefinitelyTyped manifest, then the tarball), so the whole sequence runs
    /// inside the one `spawn_blocking` — splitting it would put an `.await`
    /// between requests for no benefit.
    async fn fetch_and_store_via_spawn_blocking(
        &self,
        eco: Ecosystem,
        package: String,
        version: String,
    ) -> Result<FetchOutcome, ErrorData> {
        let outcome = match eco {
            Ecosystem::Rust => {
                let url = docs_rs_json_url(&package, &version);
                let fetched =
                    Self::blocking_fetch(eco, &package, move || UreqFetcher::new().fetch(&url))
                        .await?;
                self.with_flare_docs_store(|store| {
                    store_fetched(store, &fetched, &package, &version)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
                })
                .await?
            }
            Ecosystem::Npm => {
                let (pkg, ver) = (package.clone(), version.clone());
                let fetched = Self::blocking_fetch(eco, &package, move || {
                    npm::fetch_package(&UreqFetcher::new(), &pkg, &ver)
                })
                .await?;
                self.with_flare_docs_store(|store| {
                    npm::store_package(store, &fetched)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
                })
                .await?
            }
            Ecosystem::Python => {
                let (pkg, ver) = (package.clone(), version.clone());
                let fetched = Self::blocking_fetch(eco, &package, move || {
                    python::fetch_package(&UreqFetcher::new(), &pkg, &ver)
                })
                .await?;
                self.with_flare_docs_store(|store| {
                    python::store_package(store, &fetched)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
                })
                .await?
            }
        }?;
        self.gc_docs_cache().await;
        Ok(outcome)
    }

    /// Trims the docs cache back inside its retention and size budgets.
    ///
    /// Scheduled by the fetch rather than by a timer: every route that grows
    /// the cache funnels through the call above, so a completed fetch is
    /// exactly when there is something new to collect, and nothing has to
    /// own a background task to make it happen.
    ///
    /// On the blocking pool and detached, for the same reason the fetch
    /// itself is: the MCP runtime is single-threaded, and this is not the
    /// bounded local work the store's other call sites are. A purge can
    /// remove thousands of rows, and the first run against a database
    /// created before `auto_vacuum` was set rewrites the whole file and
    /// rebuilds the search index — cost that scales with the cache, not
    /// with what this fetch added. Inline, every other request on the
    /// runtime would wait behind it. Not awaited because the caller's answer
    /// does not depend on the result.
    ///
    /// Failures are swallowed on purpose. The caller asked for
    /// documentation, and a cache that could not be trimmed still answers
    /// the question it was asked; failing a fetch over housekeeping would
    /// trade a working feature for a disk-space concern.
    async fn gc_docs_cache(&self) {
        if self.ensure_flare_docs_store().await.is_err() {
            return;
        }
        let store = std::sync::Arc::clone(&self.flare_docs_store);
        tokio::task::spawn_blocking(move || {
            // `blocking_lock`, not `lock().await`: this closure is already
            // off the runtime, and a request waiting on the same mutex
            // awaits rather than blocking behind it.
            let guard = store.blocking_lock();
            let Some(store) = guard.as_ref() else {
                return;
            };
            let Ok(report) = store.gc(GcOpts::default()) else {
                return;
            };
            if report.purged + report.evicted == 0 {
                return;
            }
            // Journaled beside the git shim's log, for the same reason it
            // has one: a cache that drops content the caller still believes
            // is there needs a record of what went and when, or the next
            // surprising cache miss has no explanation behind it.
            if let Some(path) = flare_git_core::audit::default_path("gc.jsonl") {
                let _ = flare_git_core::audit::log_event(
                    &path,
                    &serde_json::json!({
                        "at": db_kit::ids::now(),
                        "store": "flare-docs",
                        "gc": report,
                    }),
                );
            }
        });
    }

    /// Shared timeout/panic/error plumbing for a blocking registry fetch. A
    /// miss names the other registry, so a caller that guessed wrong is told
    /// the fix instead of concluding the package does not exist.
    ///
    /// A caller-caused failure (a package name that 404s, a package with no
    /// types) comes back as `invalid_params`. Everything else is
    /// `internal_error`: timeouts, panics, 5xx, transport errors, and the
    /// retryable 4xx (408, 429) where the request was fine and the caller
    /// only needs to wait. A typo is not an outage, and reporting it as one
    /// sends the caller looking for a broken service instead of a typo.
    async fn blocking_fetch<T, E>(
        eco: Ecosystem,
        package: &str,
        f: impl FnOnce() -> Result<T, E> + Send + 'static,
    ) -> Result<T, ErrorData>
    where
        T: Send + 'static,
        E: std::fmt::Display + ClientError + Send + 'static,
    {
        tokio::time::timeout(FETCH_TIMEOUT, tokio::task::spawn_blocking(f))
            .await
            .map_err(|_| {
                ErrorData::internal_error(
                    format!("{} fetch timed out after {FETCH_TIMEOUT:?}", eco.as_str()),
                    None,
                )
            })?
            .map_err(|e| ErrorData::internal_error(format!("fetch task panicked: {e}"), None))?
            .map_err(|e| {
                // The hint asserts the package "was not found", so it may only
                // be attached when that is actually what happened. Appending it
                // to a 503, a corrupt tarball, or a package that exists but
                // ships no types tells the caller something false — and in the
                // no-types case contradicts the sentence it is appended to.
                let msg = if e.is_package_missing() {
                    format!("{e} — {}", eco.other_ecosystem_hint(package))
                } else {
                    e.to_string()
                };
                if e.is_client_error() {
                    ErrorData::invalid_params(msg, None)
                } else {
                    ErrorData::internal_error(msg, None)
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::flare_docs::{DEFAULT_LIST_LIMIT, DocUpsertOpts, MAX_LIST_LIMIT};

    fn test_mcp() -> AgentflareMcp {
        AgentflareMcp {
            flare_docs_store_override: Some(std::path::PathBuf::from(":memory:")),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn list_on_empty_store_returns_an_empty_page() {
        let mcp = test_mcp();
        let req = FlareDocsRequest {
            action: "list".to_string(),
            ..Default::default()
        };
        let result = mcp.flare_docs_impl(req).await.unwrap();
        let out: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(out["docs"].as_array().unwrap().len(), 0);
        assert_eq!(out["total"].as_u64().unwrap(), 0);
    }

    #[tokio::test]
    async fn get_by_package_reads_the_cache_without_fetching() {
        // Pre-seed the store directly (no network involved), then confirm
        // "get" returns the cached doc rather than attempting a live fetch
        // -- a live fetch in this test environment would error/hang, so a
        // successful, fast result proves the cache path was taken.
        let mcp = test_mcp();
        mcp.with_flare_docs_store(|store| {
            store
                .upsert(
                    &Ecosystem::Rust.docs_id_path("serde", "latest"),
                    "cached docs",
                    DocUpsertOpts::default(),
                )
                .unwrap()
        })
        .await
        .unwrap();

        let req = FlareDocsRequest {
            action: "get".to_string(),
            package: Some("serde".to_string()),
            ..Default::default()
        };
        let result = mcp.flare_docs_impl(req).await.unwrap();
        assert!(result.contains("cached docs"), "{result}");
    }

    #[tokio::test]
    async fn npm_get_reads_the_npm_cache_prefix_not_the_docsrs_one() {
        // The two ecosystems share one store; a cached npm package must be
        // found under `npm/...` and must not be shadowed by, or shadow, a
        // same-named crate cached under `docsrs/...`.
        let mcp = test_mcp();
        mcp.with_flare_docs_store(|store| {
            store
                .upsert(
                    &Ecosystem::Npm.docs_id_path("hono", "latest"),
                    "cached npm docs",
                    DocUpsertOpts::default(),
                )
                .unwrap()
        })
        .await
        .unwrap();

        let req = FlareDocsRequest {
            action: "get".to_string(),
            package: Some("hono".to_string()),
            ecosystem: Some("npm".to_string()),
            ..Default::default()
        };
        let result = mcp.flare_docs_impl(req).await.unwrap();
        assert!(result.contains("cached npm docs"), "{result}");
    }

    #[tokio::test]
    async fn scoped_package_names_resolve_to_npm_without_an_explicit_ecosystem() {
        let mcp = test_mcp();
        mcp.with_flare_docs_store(|store| {
            store
                .upsert(
                    &Ecosystem::Npm.docs_id_path("@types/node", "latest"),
                    "scoped npm docs",
                    DocUpsertOpts::default(),
                )
                .unwrap()
        })
        .await
        .unwrap();

        let req = FlareDocsRequest {
            action: "get".to_string(),
            package: Some("@types/node".to_string()),
            ..Default::default()
        };
        let result = mcp.flare_docs_impl(req).await.unwrap();
        assert!(result.contains("scoped npm docs"), "{result}");
    }

    #[tokio::test]
    async fn an_unknown_ecosystem_is_rejected_before_any_network_work() {
        let mcp = test_mcp();
        let req = FlareDocsRequest {
            action: "get".to_string(),
            package: Some("nokogiri".to_string()),
            ecosystem: Some("rubygems".to_string()),
            ..Default::default()
        };
        let err = mcp.flare_docs_impl(req).await.unwrap_err();
        assert!(err.to_string().contains("rubygems"), "{err}");
    }

    #[tokio::test]
    async fn a_slow_fetch_leaves_the_runtime_free_to_make_progress() {
        // Regression guard for the inline-fetch bug fixed in 83f76add, which
        // was previously only proven by an ad-hoc uncommitted script. If
        // `blocking_fetch` ever runs its closure inline instead of on
        // `spawn_blocking`, the single-threaded MCP runtime stalls for the
        // whole fetch and the concurrent ticker below cannot advance.
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        let ticks = Arc::new(AtomicUsize::new(0));
        let ticker_ticks = ticks.clone();
        let ticker = tokio::spawn(async move {
            for _ in 0..10 {
                tokio::time::sleep(Duration::from_millis(10)).await;
                ticker_ticks.fetch_add(1, Ordering::SeqCst);
            }
        });

        AgentflareMcp::blocking_fetch(Ecosystem::Rust, "serde", || {
            std::thread::sleep(Duration::from_millis(300));
            Ok::<(), ::flare_docs::FetchError>(())
        })
        .await
        .unwrap();

        // 10 ticks of 10ms each fit comfortably inside a 300ms fetch even
        // with Windows' ~15ms timer granularity; inline execution yields ~0.
        let observed = ticks.load(Ordering::SeqCst);
        assert!(
            observed >= 5,
            "runtime was blocked during the fetch: only {observed} ticks elapsed"
        );
        ticker.await.unwrap();
    }

    #[tokio::test]
    async fn a_404_is_the_callers_mistake_not_an_internal_error() {
        // A package name that does not exist is a bad argument. Reporting it
        // as `internal_error` sends the caller hunting for a broken registry.
        let err = AgentflareMcp::blocking_fetch(Ecosystem::Rust, "no-such-crate", || {
            Err::<(), _>(::flare_docs::FetchError::Status(404))
        })
        .await
        .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);

        // A 5xx genuinely is the registry failing, and must stay internal.
        let err = AgentflareMcp::blocking_fetch(Ecosystem::Rust, "serde", || {
            Err::<(), _>(::flare_docs::FetchError::Status(503))
        })
        .await
        .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INTERNAL_ERROR);
    }

    #[tokio::test]
    async fn the_try_other_ecosystem_hint_is_only_attached_to_a_real_miss() {
        // The hint's wording asserts the package "was not found", so attaching
        // it to anything else states a falsehood.
        let missing = AgentflareMcp::blocking_fetch(Ecosystem::Rust, "nope", || {
            Err::<(), _>(::flare_docs::FetchError::Status(404))
        })
        .await
        .unwrap_err();
        assert!(missing.message.contains("ecosystem=\"npm\""), "{missing:?}");

        // A package that exists but ships no types is not missing; the hint
        // would contradict the error's own message.
        let no_types = AgentflareMcp::blocking_fetch(Ecosystem::Npm, "express", || {
            Err::<(), _>(::flare_docs::npm::NpmError::Npm(
                ::flare_docs::npm::NpmFetchError::NoTypes("express".to_string()),
            ))
        })
        .await
        .unwrap_err();
        assert!(!no_types.message.contains("was not found"), "{no_types:?}");
        assert_eq!(no_types.code, rmcp::model::ErrorCode::INVALID_PARAMS);

        // Nor is a registry outage a missing package.
        let outage = AgentflareMcp::blocking_fetch(Ecosystem::Rust, "serde", || {
            Err::<(), _>(::flare_docs::FetchError::Status(503))
        })
        .await
        .unwrap_err();
        assert!(!outage.message.contains("was not found"), "{outage:?}");
    }

    /// Fills the store with `n` documents and returns the parsed `list`
    /// response for the given request tweaks.
    async fn list_with(n: usize, tweak: impl FnOnce(&mut FlareDocsRequest)) -> serde_json::Value {
        let mcp = test_mcp();
        mcp.with_flare_docs_store(|store| {
            for i in 0..n {
                store
                    .upsert(
                        // Zero-padded so the `ORDER BY path` the store applies
                        // matches numeric order, making offset assertions
                        // below say what they appear to say.
                        &Ecosystem::Rust.docs_id_path(&format!("crate{i:04}"), "latest"),
                        "docs",
                        DocUpsertOpts::default(),
                    )
                    .unwrap();
            }
        })
        .await
        .unwrap();

        let mut req = FlareDocsRequest {
            action: "list".to_string(),
            ..Default::default()
        };
        tweak(&mut req);
        serde_json::from_str(&mcp.flare_docs_impl(req).await.unwrap()).unwrap()
    }

    #[tokio::test]
    async fn list_caps_its_page_by_default_and_reports_what_it_withheld() {
        // The previous contract was "return everything unless a limit is
        // given". Per-item indexing turned that into a response no caller can
        // receive: one real cache reached 10,889 documents / 7.75 MB.
        let n = DEFAULT_LIST_LIMIT + 5;
        let out = list_with(n, |_| {}).await;

        assert_eq!(out["docs"].as_array().unwrap().len(), DEFAULT_LIST_LIMIT);
        assert_eq!(
            out["returned"].as_u64().unwrap() as usize,
            DEFAULT_LIST_LIMIT
        );
        // Without this a truncated page is indistinguishable from a complete
        // one, which is how a caller silently concludes the cache is smaller
        // than it is.
        assert_eq!(out["total"].as_u64().unwrap() as usize, n);
    }

    #[tokio::test]
    async fn list_clamps_an_oversized_limit_rather_than_rejecting_it() {
        let out = list_with(MAX_LIST_LIMIT + 5, |req| req.limit = Some(usize::MAX)).await;
        assert_eq!(out["docs"].as_array().unwrap().len(), MAX_LIST_LIMIT);
    }

    #[tokio::test]
    async fn list_offset_reaches_documents_past_the_first_page() {
        // The cap is only defensible if it bounds a response without putting
        // anything out of reach.
        let n = DEFAULT_LIST_LIMIT + 5;
        let out = list_with(n, |req| req.offset = Some(DEFAULT_LIST_LIMIT)).await;

        let docs = out["docs"].as_array().unwrap();
        assert_eq!(docs.len(), 5);
        assert_eq!(out["offset"].as_u64().unwrap() as usize, DEFAULT_LIST_LIMIT);
        assert_eq!(out["total"].as_u64().unwrap() as usize, n);
        assert!(
            docs[0]["path"].as_str().unwrap().contains("crate0100"),
            "expected the page to start where the first one ended, got {:?}",
            docs[0]["path"]
        );
    }

    #[tokio::test]
    async fn list_omits_document_bodies() {
        // The size property, stated as a property rather than a byte count:
        // a 100 KB body must not make the listing 100 KB bigger.
        let mcp = test_mcp();
        let big = "x".repeat(100_000);
        mcp.with_flare_docs_store(|store| {
            store
                .upsert(
                    &Ecosystem::Rust.docs_id_path("heavy", "latest"),
                    &big,
                    DocUpsertOpts::default(),
                )
                .unwrap();
        })
        .await
        .unwrap();

        let raw = mcp
            .flare_docs_impl(FlareDocsRequest {
                action: "list".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();

        assert!(
            raw.len() < 1_000,
            "listing carried the body: {} bytes for one document",
            raw.len()
        );
        let out: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let doc = &out["docs"][0];
        assert!(doc.get("content").is_none(), "{doc:?}");
        // Kept so a caller can still tell an empty placeholder from a real
        // page without being handed the page.
        assert_eq!(doc["content_bytes"].as_u64().unwrap(), 100_000);
    }

    #[tokio::test]
    async fn unknown_action_is_rejected() {
        let mcp = test_mcp();
        let req = FlareDocsRequest {
            action: "bogus".to_string(),
            ..Default::default()
        };
        let result = mcp.flare_docs_impl(req).await;
        assert!(result.is_err());
    }
}
