//! Library facade for the `agentflare` binary crate. Kept minimal: the code
//! in `src/code/` is shared with the binary entry point (`src/main.rs`) so
//! the core `code impact` logic can be unit- and integration-tested and,
//! later, called from an MCP tool without touching the CLI layer.

pub mod code;
