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
    ClientError, DocsStore, Ecosystem, FetchOutcome, Fetcher, UreqFetcher, docs_rs_json_url, npm,
    store_fetched,
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
                })?
            }
            "list" => self.with_flare_docs_store(|store| {
                let docs = store
                    .list()
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                serde_json::to_string(&docs)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))
            })?,
            "get" if req.id.is_some() => {
                let id = req.id.expect("guarded by is_some() above");
                self.with_flare_docs_store(|store| {
                    let doc = store
                        .get(&id)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    serde_json::to_string(&doc)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
                })?
            }
            "get" => {
                let package = req.package.ok_or_else(|| {
                    ErrorData::invalid_params("get requires \"id\" or \"package\"", None)
                })?;
                let eco = Ecosystem::resolve(req.ecosystem.as_deref(), &package)
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                let version = req.version.unwrap_or_else(|| DEFAULT_VERSION.to_string());
                let cached = self.with_flare_docs_store(|store| {
                    store
                        .get_by_path(&eco.docs_id_path(&package, &version))
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
                })??;
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
    /// inline on the single-threaded MCP runtime, and never under the
    /// `std::sync::Mutex` guarding `flare_docs_store`), then, once the fetch
    /// has completed and no `.await` remains, does the fast local
    /// parse/store work synchronously via `with_flare_docs_store`.
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
        match eco {
            Ecosystem::Rust => {
                let url = docs_rs_json_url(&package, &version);
                let fetched =
                    Self::blocking_fetch(eco, &package, move || UreqFetcher::new().fetch(&url))
                        .await?;
                self.with_flare_docs_store(|store| {
                    store_fetched(store, &fetched, &package, &version)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
                })?
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
                })?
            }
        }
    }

    /// Shared timeout/panic/error plumbing for a blocking registry fetch. A
    /// miss names the other registry, so a caller that guessed wrong is told
    /// the fix instead of concluding the package does not exist.
    ///
    /// A caller-caused failure (a package name that 404s, a package with no
    /// types) comes back as `invalid_params`; only genuine infrastructure
    /// failures — timeouts, panics, 5xx, transport errors — are
    /// `internal_error`. A typo is not an outage, and reporting it as one
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
                let msg = format!("{e} — {}", eco.other_ecosystem_hint(package));
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
    use ::flare_docs::DocUpsertOpts;

    fn test_mcp() -> AgentflareMcp {
        AgentflareMcp {
            flare_docs_store_override: Some(std::path::PathBuf::from(":memory:")),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn list_on_empty_store_returns_empty_array() {
        let mcp = test_mcp();
        let req = FlareDocsRequest {
            action: "list".to_string(),
            ..Default::default()
        };
        let result = mcp.flare_docs_impl(req).await.unwrap();
        assert_eq!(result, "[]");
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
            package: Some("requests".to_string()),
            ecosystem: Some("pypi".to_string()),
            ..Default::default()
        };
        let err = mcp.flare_docs_impl(req).await.unwrap_err();
        assert!(err.to_string().contains("pypi"), "{err}");
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
