## Summary

Asset MCP tool for attaching, fetching, listing, and deleting file attachments on items and projects.

## Changes

- **CreateAsset now has storage_path** — caller-supplied path wins, else UUID fallback
- **Path traversal guard** on staging filename rejects ".." and absolute components
- **list handles project_id** scoping (was ignored before)
- **Responses strip storage_path** from JSON output via strip_storage_path helper
- **Delete refcount fails closed** — query error propagates instead of unwrap_or(0)
- **remove_file moved after DB insert** — staging file only removed on success
- **5 unit tests** — attach→get→list→delete round trip, path traversal, param validation

## Verification

- 397 tests pass (all existing + new)
- cargo fmt --check clean
- cargo clippy --all-targets --all-features -- -D warnings clean
