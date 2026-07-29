# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.7.0](https://github.com/getappz/agentflare/compare/v1.6.0...v1.7.0) - 2026-07-29

### Added

- add flare-vault crate + adopt for gateway secrets/channel tokens/GitHub auth (#370)
- agentflare-jobs crate — background job queue + process supervisor for agent CLIs (#298)
- apply ponytail-audit findings (9 unused deps + 11 dead functions) (#330)
- auto-enforce core-module usage via init/SessionStart (#352)
- auto-release claim on cross-agent reassignment (#296)
- bound artifact version history and audit log growth (#289)
- bound the docs cache with retention, eviction, and page reclaim (#348)
- build and install PATH shims alongside the main binary (#301)
- caller-vs-service error mapping, search limit cap, non-blocking-fetch test (#344)
- case-insensitive + SQL-style SPDX in pre_filter noise patterns (#307)
- CI paths in trust-root baseline + /flare:git slash command (#367)
- clear genuine ACL denials on orphaned worktree cleanup (#322)
- close 3 ponytail parity gaps (#310)
- close branch-guard bypasses for bare filenames, new dirs, missing path fields (#291)
- close staging symlink/TOCTOU bypass, log swallowed blob-reclaim errors (#357)
- core-module auto-enforcement, docs-site, flare-docs Python/npm examples, coaching MANDATORY tier (#361)
- cover the shell layer in agentflare init (bashenv guard, opencode branch-guard, config split) (#324)
- cut the Windows build's redundant pass, Defender scanning, and PDB cost (#349)
- decouple agent-registry dependency (#297)
- dedup documents before creating the unique index (#346)
- default-limit and paginate item(list) (#358)
- deny pushing the default branch (PR-only enforcement) (#312)
- don't open a redundant PR when item done runs after the PR already merged (#329)
- drive every FTS5 index from triggers, not hand-written sync (#347)
- drop the Windows Defender exclusion step, it bought nothing (#350)
- finish EPIC #272 — Task 6 snooze/dismiss write-path + Task 9 provisioning (#354)
- flare doctor — claim-worktree health sweep + safe reclaim (#235) (#305)
- flare-docs stale-item reconciliation + doc history-skip check (#337)
- give each db its own blob dir so GC can't delete a neighbour's content (#351)
- index per-item rustdoc docs, not just the crate overview (#323)
- intent-to-skill injection + skill install CLI (#231, #233) (#290)
- log DB errors, validate cost by=, gate non-local bind, add HTTP tests (#369)
- manage a flare-docs usage rule + add `agentflare doctor` (#325)
- never let doctor reclaim delete the main worktree (#327)
- npm/TypeScript ecosystem support; rename tool to `docs` (#342)
- on-demand third-party docs — flare-docs crate + flare_docs MCP tool + agentflare docs CLI (#316)
- only block trust-root pushes to the default branch (#317)
- path-scope enforcement for claims (QuorumGit adoption) (#303)
- Python ecosystem, README usage examples, and the docs site
- regex pre-filter for ponytail over-engineering checks (#293)
- release blob refs when documents are deleted (#343)
- rename ponytail_engineering_check_internal to over_engineering_check_internal (#300)
- repo-local verify gate + pr_wait primitive (#118) (#362)
- request-optimization short-circuits for CLI bookkeeping calls (#355)
- reset recursion depth before exec'ing real git (#359)
- resilient worktree removal on Windows file-lock (#302) (#308)
- restyle statusline badge to Claude Code native hint style (#311)
- retire OpenResearch, fold 17-source search into flare_search (#326)
- reuse an existing branch that owns no worktree instead of failing (#321)
- rivalsearch param name + store FTS query sanitization (#288)
- scope all classify.rs denials to agentflare-tracked projects (#320)
- share sccache across worktrees when available (#133) (#299)
- shared nudge-pacing primitive for coaching rules + PostToolUseFailure hook (#364)
- skill routing/management epic — FTS5, negation, bandit ranking, pack/hub lifecycle (#302)
- split skill tool handler into its own module (#319)
- stop `list` returning every cached document in full (#345)
- stop usetsearch blocking every native ToolSearch call (#368)
- surface worktree creation failure reason in claim response (#318)
- sweep to reclaim orphaned legacy shared blobs (#353)
- tune .coderabbit.yaml to reduce review rate-limit hits (#309)
- unified config.toml git-shim policy slice (#331) (#336)
- use plain tag ref for SLSA provenance reusable workflow (#314)
- use plain tag ref for SLSA provenance reusable workflow (#315)
- v1.6.0 verification bugs — opencode filePath guard hole, search web/store arms (#287)
- validate action before repo/client setup; resolve default branch via API (#363)
- vent tag union + flare-docs fetch-outcome observability (#338)
- verified continuation commit, structured payload, duplicate-item reuse, assignee freeze (#365)
- warn when editing on a branch stale vs origin/default (#360)
- worktree orphan audit + fix git_binary shims-dir self-deny (#304)

### Added

- `flare-vault` crate + `agentflare vault` CLI — local secrets vault with Argon2id/AES-256-GCM envelope encryption, OS-keyring session cache, and global/project scoping; replaces the sqlite-backed `gateway_secrets` store used by `agentflare gateway secret`, channel bot tokens, and GitHub auth (#405)
- `flare_git` `pr_wait` action — bounded server-side poll loop for PR checks (default 60s, capped 120s per call), replacing manual `gh pr checks` polling loops (#118)

## [1.6.0](https://github.com/getappz/agentflare/compare/v1.5.0...v1.6.0) - 2026-07-21

### Added

- global search MCP tool — store, memory, code, and web arms, with gateway delegation and local MCP auto-registration (#284)
- migrate asset store to agentflare-store documents+blobs (#282)
- git-aware PATH shim — classify, snapshot, canonical-repo detach guard (#279)
- cover opencode's native tool names in the branch guard (#281)
- release-bump.sh + release-tag.sh — manual release flow, no crates.io (#275)

### Fixed

- resolve PreToolUse branch guard against target file's repo, not host cwd (#283)
- sync session hooks with actual feature state, cut per-turn bloat (#277)
- don't run git worktree prune under --dry-run (#272)

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
