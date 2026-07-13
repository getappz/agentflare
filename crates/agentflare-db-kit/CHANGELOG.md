# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Initial crate scaffold: `open_file`/`open_memory` (SQLite connection setup + `rusqlite_migration`-backed migrations), `ids::now`/`ids::new_id`, and a generic `ClaimLedger` (composite-key-aware leased-claim ledger) factored out of `src/claims.rs`
