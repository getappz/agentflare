pub mod fetch;
pub mod rustdoc;
pub mod store;

pub use agentflare_store::documents::{DocMatch, DocUpsertOpts, Document};
pub use fetch::{FetchError, FetchedBytes, Fetcher, UreqFetcher};
pub use rustdoc::{RustdocError, docs_rs_json_url, fetch_and_store, store_fetched};
pub use store::{DocsStore, Error, PROJECT_ID};
