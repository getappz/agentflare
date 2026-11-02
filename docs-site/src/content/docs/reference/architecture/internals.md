---
title: Internals
description: Subsystem map, runtime and deployment surface, dependency lifecycle, storage, APIs, extensions, and background jobs.
---

## Directory and subsystem map

Directory names below are source locations, which sometimes differ from Cargo
package names (`flare-code` is packaged as `agentflare-flare-code`, for example).
[Root dependency aliases](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/Cargo.toml#L78).

| Area | Responsibility and primary source |
|---|---|
| `src/cli/`, `src/main.rs` | Command parsing and dispatch into application modules. [dispatch](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/mod.rs#L192) |
| `src/mcp_server/`, `src/mcp_server.rs` | Tool adapters, request types, project scoping, lazy registries/stores, stdio/HTTP transport. [server state](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server.rs#L127) |
| `src/init.rs`, `src/components.rs`, `src/hook*.rs`, `hooks/`, `.codex-plugin/` | Host configuration, lifecycle callbacks, guardrails, optional plugin packaging. [init](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/init.rs#L92), [hooks](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/hook.rs#L467) |
| `src/optimize/`, `crates/flare-code`, `crates/flare-output` | Code-minimalism instructions, prose compression, context scoring, reversible output retrieval and runtime nudges. [CLI surface](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/mod.rs#L86), [context scorer](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/optimize/context.rs#L26) |
| `src/coaching/`, `src/memory/` | Persistent/contextual rules, observations, session memory, recall and evidence-backed lessons. [hook integration](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/hook.rs#L489), [recall](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/memory/search.rs#L106) |
| `crates/agentflare-backend` | Workspaces, projects, states, items, comments, labels, assignments, bridge registration and webhooks. [module inventory](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-backend/src/lib.rs), [schema registration](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-backend/src/db.rs#L11) |
| `src/claims.rs`, `src/sessions.rs`, `src/messages/`, `src/handoff/`, `src/review/` | Runtime ownership, live-session addressing, messaging, handoffs and review consensus. [DB migrations](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/db.rs#L21), [review core](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/review.rs#L1) |
| `src/work_item_pipeline*`, `src/supervisor/`, `src/cli/work/` | Item execution, SDD role loop, finalization, discovery and PR review/repair policy. [pipeline](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/work_item_pipeline.rs#L1103), [supervisor](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/supervisor.rs) |
| `crates/flare-workflow`, `src/workflow*`, `src/schedule/` | Generic workflow engine, JSON definitions, durable journal, execution leases, intervals and occurrence receipts. [engine](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/flare-workflow/src/engine.rs#L196), [schedule](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/schedule/mod.rs#L1) |
| `crates/agentflare-jobs`, `crates/agentflare-resource-gate` | Persistent queue, subprocess/in-process executors, worker pool and resource-aware dispatch. [job types](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-jobs/src/types.rs#L7), [pool boot](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/dashboard/dispatch_boot.rs#L19) |
| `crates/flare-git-core`, `flare-git-shim`, `agentflare-shim`; `src/worktree/` | Local Git/worktree primitives, policy/provenance and PATH shims. `src/github/` separately owns remote GitHub APIs and bridge logic. [boundary](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/flare-git-core/src/lib.rs#L1), [entry imports](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/main.rs#L45) |
| `crates/flare-sandbox`, `flare-process`, `agentflare-approval` | Process launch utilities, platform-dependent isolation and persisted human approval decisions. [sandbox](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/flare-sandbox/src/lib.rs#L1), [approval gate](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-approval/src/lib.rs#L1) |
| `crates/agentflare-store`, `agentflare-db-kit`, `flare-search-kit` | Documents/blobs/KV/vectors, connection/migration helpers, shared search utilities. [store API](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-store/src/lib.rs#L1), [open helper](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-db-kit/src/open.rs#L1) |
| `crates/agent-registry`, `skill-registry`, `gateway-registry` | Agent registry, filesystem skill discovery/search, downstream MCP tool discovery and invocation. [manifest aliases](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/Cargo.toml#L78), [gateway exports](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/gateway-registry/src/lib.rs#L17) |
| `crates/flare-docs` | Separately cached third-party API documentation; independent lifecycle from project documents. [store owner](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server.rs#L209) |
| `crates/agentflare-artifacts`, `src/artifacts.rs`, `src/asset_store.rs` | Versioned artifact publishing/serving and project assets. [lazy artifact backend](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server.rs#L147), [artifact routes](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/dashboard/artifacts.rs#L98) |
| `src/auth*`, `src/vault.rs`, `crates/flare-vault` | Auth profiles/rotation and encrypted project secrets, with distinct legacy/new vault paths. [auth cryptography](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/auth_crypt.rs#L1), [vault wrapper](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/vault.rs#L3) |
| `crates/flare-proxy`, `flare-consensus`, `flare-consensus-proxy` | Provider proxy and consensus-related libraries. Proxy HTTP is mounted by the dashboard; provider configuration can be supplied explicitly. [proxy](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/flare-proxy/src/lib.rs#L18), [workspace](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/Cargo.toml#L2) |
| `crates/flare-browser`, `src/browser_install.rs`, `src/browser_decision.rs` | Browser command layer, first-use backend installation and observed-action decisions. [installer](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/browser_install.rs#L26), [CLI](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/browser.rs) |
| `crates/flare-insights`, `src/cost.rs`, `src/rollup.rs` | Session ingest/search/replay/export and token/cost reporting. Insights explicitly lists Claude/Codex/OpenCode adapters, with other hosts incomplete. [insights modules](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/flare-insights/src/lib.rs#L6) |
| `crates/agentflare-apps`, `apps/auto-company`, `src/cli/apps*` | App manifests, tools manifests and domain workflows using the workflow sender. [exports](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-apps/src/lib.rs), [app sender](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/workflow.rs#L276) |
| `crates/agentflare-config`, `agentflare-bin-lib`, `agentflare-workspace-hack` | Shared paths/config, binary support, generated dependency unification. [config API](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-config/src/lib.rs), [hakari gate](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/ci.yml#L201) |
| `dashboard/web`, `docs-site`, `site` | Embedded application UI, separately built docs, public Worker website. [embedded UI](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/dashboard/server.rs#L1), [JS workspace](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/aube-workspace.yaml#L1) |
| `tests`, `.config`, `.githooks`, `.github`, `scripts` | Test executables/configuration, Git hooks, CI/release policy and maintenance scripts. [nextest config](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.config/nextest.toml), [pre-commit](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.githooks/pre-commit#L1) |

## Runtime and deployment surface

| Surface | Pin / selection | Implication and evidence |
|---|---|---|
| Native developer/build compiler | `rust-toolchain.toml`: **1.98.0**; manifest minimum **1.91** | Distinct toolchain pin and declared minimum; minimum-version compatibility was not tested. [toolchain](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/rust-toolchain.toml#L1), [manifest](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/Cargo.toml#L12) |
| CI compilation and linting | Build uses repository toolchain; fmt/clippy explicitly install floating `stable`; runner OS labels float | Potential version drift between compiler/linter environments is visible in [CI](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/ci.yml#L80), [clippy setup](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/ci.yml#L167) |
| Native releases | Linux GNU x86_64/aarch64, macOS x86_64/aarch64, Windows MSVC x86_64; semantic enabled | Linux ARM uses `cross`; no Linux musl or Windows ARM build in this matrix. [release matrix](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/release.yml#L15) |
| Container | `debian:13-slim`, no digest; installer fetched from `master`; non-root user | The Dockerfile installs a release at build time, rather than building the checked-out Rust tree. Image tags alone do not pin the embedded binary version. [Dockerfile](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/Dockerfile#L1), [publish workflow](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/docker.yml#L3) |
| Public Worker | `compatibility_date = 2025-04-01`; Wrangler `^4`; custom domains | Separate deployable from the native daemon. [config](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/site/wrangler.jsonc#L1), [manifest](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/site/package.json) |
| Documentation JS runtime | Astro/Starlight dependency ranges; aube `latest` in mise | No Node `engines` pin in the inspected root/site/docs manifests; aube lockfile records package resolution, not a fixed host runtime. [mise](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/mise.toml#L1), [docs manifest](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/docs-site/package.json) |
| Browser backend | `github:vercel-labs/agent-browser@latest` via mise; browser fetched by backend `install` | First use needs external downloads unless already provisioned. [installer](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/browser_install.rs#L19) |
| Database services | Bundled SQLite files, no external DB image required for the main binary | Do not infer PostgreSQL/MySQL requirements from the excluded `flare-db` libraries. [manifest](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/Cargo.toml#L3), [SQLite dependency](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/Cargo.toml#L66) |

## Dependency lifecycle and limitations

**[UNVERIFIED] EOL/maintenance status:** local manifests and lockfiles establish
versions, not present-day vendor support. No online lifecycle/advisory audit was
performed, and this document does not claim all dependencies are maintained or
free of vulnerabilities. Existing `KNOWN_ISSUES.md` dependency-health statements
are historical assertions, not current verification. The executable checks are
cargo-deny/cargo-audit; the latter includes a carefully scoped advisory exception.
[Security workflow](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/security-check.yml#L23).

Concrete limitations visible in code:

- The compatibility `PreCompact` hook does nothing; it cannot make Claude Code
  retain scored transcript context. The standalone scorer still exists.
  [hook](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/hook.rs#L685), [scorer](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/optimize/context.rs#L26).
- Process isolation is enforced through Linux bubblewrap when available.
  Non-Linux `try_wrap` returns the plain command unless fail-closed policy is
  enabled, in which case it errors. This layer does not provide a Windows/macOS
  sandbox. [implementation](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/flare-sandbox/src/lib.rs#L99).
- Daemon autostart installation supports macOS and Linux; other platforms return
  “autostart not supported”. This differs from manually starting the daemon.
  [autostart](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/daemon_autostart.rs#L4).
- Shared worktree `node_modules` use hard links with copy fallback and a matching
  lockfile requirement. In-place writes from an unsandboxed sharer can affect
  every sharer; the managed sandbox overlay is the protection.
  [dependency store](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/flare-git-core/src/deps.rs#L1).
- Memory vector search scans and sorts eligible embeddings; it is not an ANN
  index. Recall scaling should be measured against actual observation volume.
  [candidate search](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/memory/embeddings.rs#L50).
- Work-item boot recovery uses default timeouts/extra arguments because the
  original dispatch values are not persisted. A failed workflow smoke test
  leaves the dashboard up with work dispatch off; schedule ticking is separate.
  [boot](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/dashboard/server.rs#L796), [ticker](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/dashboard/dispatch_boot.rs#L42).
- Job logs retain at most 64 MiB per stream; an explicit truncation marker is the
  indication, not a dedicated `JobOutput` flag. [job output](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-jobs/src/types.rs#L123).
- Coordination CLI/MCP/storage compatibility is still beta; a 1.x package version
  is not a whole-product stability contract. [STATUS.md](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/STATUS.md#versioning).

## Storage, APIs, extensions and background jobs

Paths below use `~/.agentflare` as the logical application home. Shared path
helpers include an override for isolated tests; platform-specific cache/data
locations are not necessarily under that directory.
[Paths](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-config/src/lib.rs#L14),
[binary support](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-bin-lib/src/paths.rs#L6).

| Store | Owner / contents / lifecycle |
|---|---|
| `backend.db` | Workspaces/projects/items/states/labels/comments and related domain state; separate append-only migration list. [open site](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server.rs#L1057), [migrations](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-backend/src/db.rs#L5) |
| `agentflare.db` | Claims, sessions, messages, review findings, gateway secrets and job queue. Each subsystem owns its tables. The file's “single source of truth” comment applies to these relational domains, not every database in the product. [dispatcher](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/db.rs#L1), [queue open](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/dashboard/server.rs#L778) |
| `store.db`, `blobs-store/` | Documents, content-addressed blobs, vectors, KV, approvals and leases. Independent migrations; blob directory is namespaced by DB stem. [store path](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-bin-lib/src/store.rs#L4), [Store](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-store/src/lib.rs#L39) |
| `memory/brain.db` | Persistent memory observations, relations, sessions and summaries; WAL, normal synchronous mode and foreign keys. [open](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/memory/store.rs#L4), [modules](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/memory/mod.rs) |
| `workflows.db` | Generic/work-item workflow state and journals; interval schedules, occurrences and durable start receipts. [default path](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/workflow.rs#L54), [scheduler tables](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/schedule/mod.rs#L1) |
| `flare-docs.db` | Third-party package documentation cache, intentionally separate from project documents and with a separate blob namespace. [owner](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server.rs#L209), [blob layout](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-store/src/lib.rs#L42) |
| Skill/gateway indexes | Lazily opened registries; filesystem/tool-derived indexes have refresh/cache lifecycles. These are not the item backend. [server fields](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server.rs#L127), [gateway API](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/gateway-registry/src/lib.rs#L17) |
| `vault.json`, auth profile files, runtime/config files | Secret storage and application state are not all SQL. Vault operations lock read/modify/write across processes. [vault](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/vault.rs#L18), [vault path](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/vault.rs#L77), [state support](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-bin-lib/src/state.rs) |

DB-kit opens files with a 5-second busy timeout, WAL, normal synchronous mode,
foreign-key enforcement and caller-owned migrations. It refuses databases newer
than the migration list known to the current binary (`SchemaAhead`); manually
rewriting `user_version` is not a schema downgrade.
[Implementation](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-db-kit/src/open.rs#L16).

MCP wraps the application in per-tool adapters and typed requests. Downstream
gateway integrations support stdio and HTTP MCP backends; skills supply
filesystem `SKILL.md` procedures; apps provide manifest/tools-driven workflow
modules. These are distinct extension mechanisms, not dynamically loaded Rust
plugins. [gateway exports](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/gateway-registry/src/lib.rs#L17),
[app exports](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-apps/src/lib.rs), [application adapter](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/workflow.rs#L276).

Background execution belongs to the dashboard/daemon process: orphan
reconciliation, workflow recovery, job workers, discovery/review/approval sweeps,
resource sampling, schedule ticks, log cleanup, stale-job detection and binary
staleness restart. Stdio MCP can administer local data without that process,
but queued schedules require it to execute.
[Boot sequence](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/dashboard/server.rs#L770),
[work dispatch](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/dashboard/dispatch_boot.rs#L19),
[schedule ticker](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/dashboard/dispatch_boot.rs#L42).

---

Previous: [Installation and verification](/docs/reference/architecture/installation-and-verification/) | Next: [Checkout and ecosystem](/docs/reference/architecture/checkout-and-ecosystem/) · [Architectural blueprint](/docs/reference/architecture/blueprint/)

