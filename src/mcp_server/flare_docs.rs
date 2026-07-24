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
    DocsStore, Fetcher, UreqFetcher, docs_id_path, docs_rs_json_url, store_fetched,
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
                let version = req.version.unwrap_or_else(|| DEFAULT_VERSION.to_string());
                let cached = self.with_flare_docs_store(|store| {
                    store
                        .get_by_path(&docs_id_path(&package, &version))
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
                })??;
                match cached {
                    Some(doc) => serde_json::to_string(&doc)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None)),
                    None => {
                        self.fetch_and_store_via_spawn_blocking(package, version)
                            .await
                    }
                }
            }
            "refresh" => {
                let package = req.package.ok_or_else(|| {
                    ErrorData::invalid_params("refresh requires \"package\"", None)
                })?;
                let version = req.version.unwrap_or_else(|| DEFAULT_VERSION.to_string());
                self.fetch_and_store_via_spawn_blocking(package, version)
                    .await
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown action \"{other}\" (expected search|get|list|refresh)"),
                None,
            )),
        }
    }

    /// Runs the docs.rs network fetch on tokio's blocking thread pool (never
    /// inline on the single-threaded MCP runtime, and never under the
    /// `std::sync::Mutex` guarding `flare_docs_store`), then, once the fetch
    /// has completed and no `.await` remains, does the fast local
    /// decompress/parse/store work synchronously via `with_flare_docs_store`.
    async fn fetch_and_store_via_spawn_blocking(
        &self,
        package: String,
        version: String,
    ) -> Result<String, ErrorData> {
        let url = docs_rs_json_url(&package, &version);
        let fetched = tokio::time::timeout(
            FETCH_TIMEOUT,
            tokio::task::spawn_blocking(move || {
                let fetcher = UreqFetcher::new();
                fetcher.fetch(&url)
            }),
        )
        .await
        .map_err(|_| {
            ErrorData::internal_error(
                format!("docs.rs fetch timed out after {FETCH_TIMEOUT:?}"),
                None,
            )
        })?
        .map_err(|e| ErrorData::internal_error(format!("fetch task panicked: {e}"), None))?
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        self.with_flare_docs_store(|store| {
            let doc = store_fetched(store, &fetched, &package, &version)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            serde_json::to_string(&doc).map_err(|e| ErrorData::internal_error(e.to_string(), None))
        })?
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
                    &docs_id_path("serde", "latest"),
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
