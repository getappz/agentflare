pub mod fetch;
pub mod rustdoc;
pub mod store;

pub use agentflare_store::documents::{DocMatch, Document, DocUpsertOpts};
pub use fetch::{Fetcher, FetchedBytes, FetchError, UreqFetcher};
pub use rustdoc::{docs_rs_json_url, fetch_and_store, RustdocError};
pub use store::{DocsStore, Error, PROJECT_ID};
