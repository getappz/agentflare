//! Shared SQLite boilerplate for agentflare's crates: connection-open +
//! migration setup, id/timestamp helpers, and a generic leased-claim ledger.
//! Deliberately narrow — not an ORM, no query layer, no schema modeling. A
//! leaf crate: never depends back on the main `agentflare` crate.

pub mod claim;
pub mod ids;
pub mod open;

pub use open::{open_file, open_memory};
