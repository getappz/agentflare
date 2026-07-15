# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/getappz/agentflare/compare/ponytail-v0.1.0...ponytail-v0.1.1) - 2026-07-12

### Fixed

- *(ponytail)* serialize state tests that race on shared global files

### Other

- add clippy, fmt, and cargo-deny gates behind a CI Green aggregator ([#158](https://github.com/getappz/agentflare/pull/158))

## [0.1.0](https://github.com/getappz/agentflare/releases/tag/ponytail-v0.1.0) - 2026-07-08

### Added

- *(ponytail)* custom skills system + mode shortcuts ([#90](https://github.com/getappz/agentflare/pull/90))
- *(ponytail)* per-session mode + status report ([#87](https://github.com/getappz/agentflare/pull/87))
- *(ponytail)* playbook skill + review scoping ([#85](https://github.com/getappz/agentflare/pull/85))
- *(ponytail)* regex-free over-engineering detector + numbered review findings ([#84](https://github.com/getappz/agentflare/pull/84))
- *(ponytail)* persona persona boundary + simplification markers ([#83](https://github.com/getappz/agentflare/pull/83))
- *(ponytail)* AGENTS.md fallback + persona hardening + anti-hallucination ([#81](https://github.com/getappz/agentflare/pull/81))

### Fixed

- close ponytail parity gaps from upstream PR audit ([#61](https://github.com/getappz/agentflare/pull/61)) ([#96](https://github.com/getappz/agentflare/pull/96))
- post-1.0.0 code review — ponytail custom skills, auth health scoring, CI defects ([#94](https://github.com/getappz/agentflare/pull/94))
- *(ponytail)* E0593 compile error + restore AGENTS.md fallback lost in #90 ([#93](https://github.com/getappz/agentflare/pull/93))
- *(ponytail)* remove stale conflict markers in instructions.rs
- *(ponytail)* Codex hook output — additionalContext at top level, not nested ([#89](https://github.com/getappz/agentflare/pull/89))

### Other

- multi-crate workspace + mise-style CLI
