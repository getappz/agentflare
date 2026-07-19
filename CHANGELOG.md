# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.5.0](https://github.com/getappz/agentflare/compare/v1.4.0...v1.5.0) - 2026-07-20

### Added

- `agentflare daemon {start,stop,restart,status,enable,disable}`: background daemon lifecycle (PID file, flock-based start lock, Unix socket / Windows named pipe IPC), autostart registration via launchd (macOS) / systemd --user (Linux), and a 24h-cached update check. HTTP-over-IPC client and macOS ad-hoc codesign scaffolding land ahead of the follow-up that wires the daemon's own HTTP handler and MCP tool-call dispatch.
- `flare-proxy` crate: Anthropic-to-OpenAI free-provider proxy, with env-var model routing (`MODEL`/`MODEL_OPUS`/`MODEL_SONNET`/`MODEL_HAIKU`).
- `@mention` feature: inline `@I`/`@A`/`@search` references resolved across items, agents, and search.
- `agentflare work`: autonomous claim → worktree → headless agent → report-back command.
- `vent` MCP tool + `agentflare vent` CLI: agents log tooling friction to an append-only per-repo JSONL; a deterministic classifier consolidates them once per turn (via the PromptSubmit hook) and auto-files actionable vents as backlog items. No new dependencies; fully auditable (raw `vents.jsonl` + `vent list`).
- Read-only dashboard (`agentflare serve`), Phases 0-2.
- `flare_git`: paginated list actions (no more silent 30-item truncation) and a `pr_status` action bundling PR detail + CI checks + reviews + comments into one call.
- `flare_handoff`: knowledge fact import + session snapshot on handoff.
- `agentflare-store` crate: initial KV/document (CRUD+FTS+vector+hybrid)/blob/lease engine.
- CLI branding banner (logo asset, installers, `about` command).

### Changed

- *(memory)* brain.db now opens through the shared db-kit engine (versioned migrations, WAL, FK enforcement); recall gains optional hybrid semantic search (BM25+vector merge, 30-day temporal decay, MMR) behind `--features semantic`, with `agentflare memory backfill-embeddings` to index existing observations. FTS-only behavior is byte-identical without an embedding model.
- `item`/`claim` tools accept a numeric `sequence_id` directly; new item IDs switch to nanoid.
- CI: LOC-gate wired into the pre-commit hook (staged files only).

## [1.4.0](https://github.com/getappz/agentflare/compare/v1.3.1...v1.4.0) - 2026-07-17

### Added

- *(github)* `flare_git` GitHub module — PR/issue/release/workflow-run models, auth token resolution, action-dispatch MCP tool, init-auth credential classifier ([#221](https://github.com/getappz/agentflare/pull/221))
- *(ui)* adopt cliclack for interactive CLI prompts and status output ([#220](https://github.com/getappz/agentflare/pull/220))
- *(optimize)* reversible-compression retrieve registry + `optimize retrieve` CLI/MCP actions (CCR)
- *(pm)* PM skill pack v1 — /pm:standup /pm:groom /pm:plan /pm:health
- *(coaching)* contextual coaching triggers (BM25 auto-match) ([#213](https://github.com/getappz/agentflare/pull/213))
- *(labels)* MCP list/update/delete + project-scope enforcement on attach ([#205](https://github.com/getappz/agentflare/pull/205))
- MCP-safe self-upgrade primitive + `agentflare dev-install` ([#206](https://github.com/getappz/agentflare/pull/206))
- *(maintainability)* adopt LOC gate from lean-ctx ([#218](https://github.com/getappz/agentflare/pull/218))
- server-side groom/standup/health/plan actions

### Fixed

- *(gateway)* init idempotency check survives malformed sibling entries ([#219](https://github.com/getappz/agentflare/pull/219))
- *(github)* validate workflow_dispatch inputs, percent-encode query values, detect stored github_token, retry RateLimited instead of erroring
- *(init)* guarantee a GitHub credential for github repos; skip PAT prompt under -y / non-TTY stdin
- *(mcp)* return text asset content as UTF-8, not base64; server-derive artifact sender ([#211](https://github.com/getappz/agentflare/pull/211), [#207](https://github.com/getappz/agentflare/pull/207))
- *(worktree)* skip PR when branch content already merged (squash detection); close ambient CARGO_TARGET_DIR gap for agent builds and CI ([#210](https://github.com/getappz/agentflare/pull/210), [#217](https://github.com/getappz/agentflare/pull/217))
- *(optimize)* persist retrieve originals in blob store; atomic+locked index; TTL-on-list; resolve clippy -D warnings; address CodeRabbit review (pct panic, list path leak, legacy CCR)

### Other

- site: agentflare.dev landing page + Cloudflare Workers deploy, self-hosted display font, OG social image
- refactor: consolidate compression into the `optimize` module; fold runtime submodule in
- hook: deprecate inert PreCompact FTS5 scorer, keep as no-op stub
- chore: fix pre-existing rustfmt drift; add opencode.json rust-analyzer config; exclude machine-local opencode.json and docs/

## [1.3.0](https://github.com/getappz/agentflare/compare/agentflare-v1.2.0...agentflare-v1.3.0) - 2026-07-12

### Added

- *(hooks)* dynamic memory nudge, agentflare: prefix, auto-detect agent
- *(agents)* headless agent invocation — run a prompt, capture the reply ([#151](https://github.com/getappz/agentflare/pull/151))
- *(init)* detect GitHub repos and register github-mcp-server behind the gateway

### Fixed

- *(mcp)* register memory tools with the tool_router so they're reachable
- *(headless)* use kill -s KILL -- <pid> to avoid CLI arg-parsing ambiguity
- *(run)* reject --print combined with --model/--mode/--env/trailing args instead of silently ignoring them — the headless path never threaded those through, so users had no signal their flags were dropped.
- *(headless)* kill the whole process tree on timeout, not just the direct child — a descendant holding the stdout pipe open (e.g. a grandchild spawned by claude -p / codex exec) could hang the reader thread forever, defeating the timeout entirely.
- *(init)* only print gateway follow-up note when registration succeeded
- *(init)* make gateway register() self-idempotent, not just caller-guarded

### Other

- add clippy, fmt, and cargo-deny gates behind a CI Green aggregator ([#158](https://github.com/getappz/agentflare/pull/158))
- address CodeRabbit findings on the engram-removal commit
- remove engram integration — replaced by built-in memory module
- Merge remote-tracking branch 'origin/master' into refactor/db-consolidate-secrets
- Merge remote-tracking branch 'origin/master' into feat/review-consensus
- cap build/test job at 25 min so a hung test fails fast instead of pinning a runner for 6h
- Merge remote-tracking branch 'origin/master' into feat/claim-ledger

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
