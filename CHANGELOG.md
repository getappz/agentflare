# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.2.0](https://github.com/getappz/agentflare/compare/agentflare-v1.1.0...agentflare-v1.2.0) - 2026-07-08

### Added

- skill registry MCP — skill_search + skill_load ([#92](https://github.com/getappz/agentflare/pull/92))
- *(ponytail)* per-session mode + status report ([#87](https://github.com/getappz/agentflare/pull/87))
- *(ponytail)* SubagentStart agent_type regex matcher ([#91](https://github.com/getappz/agentflare/pull/91))
- detect competing compression plugins during init ([#86](https://github.com/getappz/agentflare/pull/86))
- agent-detector process-tree detection + auto-wire ponytail hooks
- ponytail L1 integration — port runtime to Rust
- add apt PPA and Docker distribution channels
- add --reload-daemon and shallow profile isolation (#35, #36) ([#41](https://github.com/getappz/agentflare/pull/41))
- add eyre + color-eyre for rich error reporting ([#40](https://github.com/getappz/agentflare/pull/40))
- add thiserror typed errors (partial - auth, auth_runner) ([#39](https://github.com/getappz/agentflare/pull/39))
- adopt mise conventions - build info, edition 2024, lints, tooling ([#38](https://github.com/getappz/agentflare/pull/38))
- auth vault phases 3+4 - failover, isolation, encryption ([#23](https://github.com/getappz/agentflare/pull/23))
- auth vault phase 2 - rotation, cooldown, health scoring ([#23](https://github.com/getappz/agentflare/pull/23))
- add auth_db SQLite layer for health, cooldown, rotation state
- add auth profile vault (Phase 1, addresses #23)

### Fixed

- close ponytail parity gaps from upstream PR audit ([#61](https://github.com/getappz/agentflare/pull/61)) ([#96](https://github.com/getappz/agentflare/pull/96))
- post-1.0.0 code review — ponytail custom skills, auth health scoring, CI defects ([#94](https://github.com/getappz/agentflare/pull/94))
- remove hard-coded cryptographic salt (LEGACY_SALT)
- add SAFETY docs to unsafe set_var/remove_var blocks
- *(hook)* stdin timeout + stderr logging + bare /agentflare report ([#80](https://github.com/getappz/agentflare/pull/80))
- resolve all zizmor errors on master
- add .gitmodules for winget-pkgs submodule reference
- remove no-stale-brand job - pre-existing submodule corruption causes checkout cleanup failure (winget-pkgs/ phantom reference)
- correct sccache-action SHA
- pin all actions to commit SHAs, tighten release permissions
- CI - zizmor PR-only, security-check path filter, concurrency guard
- review fixes - encryption, format marker, Windows env, retry backoff
- add actions:write permission for nested workflow dispatch

### Other

- allow manual dispatch of release-plz workflow ([#97](https://github.com/getappz/agentflare/pull/97))
- *(cla)* skip the job entirely for maintainer and bot PRs ([#95](https://github.com/getappz/agentflare/pull/95))
- add .gitattributes for cross-platform CRLF handling ([#82](https://github.com/getappz/agentflare/pull/82))
- multi-crate workspace + mise-style CLI
- Revert "fix: remove accidental winget-pkgs submodule - manifests are in winget/"
- add winget auto-update workflow using komac
- add winget manifests for v1.1.0
- auth vault phase 2 implementation plan
- auth vault phase 2 design spec
- scoop manifest: agentflare 1.1.0

## [1.1.0](https://github.com/getappz/agentflare/compare/v1.0.2...v1.1.0) - 2026-07-06

### Added

- add agentflare alias command (closes #25)

### Other

- disable git release in release-plz (handled by release.yml)
