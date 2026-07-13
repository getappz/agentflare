# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/getappz/agentflare/compare/agentflare-artifacts-v0.1.0...agentflare-artifacts-v0.1.1) - 2026-07-12

### Other

- add clippy, fmt, and cargo-deny gates behind a CI Green aggregator ([#158](https://github.com/getappz/agentflare/pull/158))

## [0.1.0](https://github.com/getappz/agentflare/releases/tag/agentflare-artifacts-v0.1.0) - 2026-07-11

### Added

- *(artifacts)* handoff envelope, version diff, git provenance, dedupe, search
- *(artifacts)* version history, CAS updates, md/mermaid rendering, LAN sharing
- *(artifacts)* add agentflare artifacts serve CLI, fix tests
- *(hook)* add SessionEnd hook

### Fixed

- *(artifacts)* honor --port flag, error on unavailable port
