pub mod ecosystem;
pub mod fetch;
pub mod index_types;
pub mod npm;
pub mod python;
pub mod readme;
pub mod rustdoc;
pub mod store;

pub use agentflare_store::documents::{DocMatch, DocUpsertOpts, Document};
pub use ecosystem::Ecosystem;

/// What a fetch produced, for every ecosystem.
///
/// Per-item indexing is best-effort on top of the overview doc (see
/// `rustdoc::store_fetched` and `npm::store_package` for why an item-level
/// parse failure must not fail the whole fetch), but that must not mean
/// invisible -- previously such a failure only reached an `eprintln!` on the
/// MCP server's own stderr, which no caller (CLI, MCP client, tests) could
/// ever observe.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FetchOutcome {
    #[serde(flatten)]
    pub doc: Document,
    /// Number of per-item docs written by this fetch (inserted or changed;
    /// see `DocsStore::upsert_batch` -- a refetch with no item content changes
    /// reports 0 here even though indexing ran).
    pub items_indexed: usize,
    /// Set when per-item indexing failed (e.g. a rustdoc `ItemKind` newer than
    /// the pinned `rustdoc-types` recognizes, or an unparseable `.d.ts`).
    /// `items_indexed` is 0 in that case. `doc` is unaffected either way.
    pub items_error: Option<String>,
}
pub use fetch::{ClientError, FetchError, FetchedBytes, Fetcher, UreqFetcher};
pub use rustdoc::{RustdocError, docs_id_path, docs_rs_json_url, fetch_and_store, store_fetched};
pub use store::{
    DEFAULT_LIST_LIMIT, DocsStore, Error, MAX_LIST_LIMIT, MAX_SEARCH_LIMIT, PROJECT_ID,
};
