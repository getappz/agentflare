---
title: Overview
description: Purpose, boundaries, capabilities, detected stack, and process ownership of agentflare.
---

## Purpose and boundaries

Agentflare combines optimization for individual coding-agent sessions with a
local coordination system for work items, agent execution, review, messaging,
and durable workflows. The [README](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/README.md#what-this-is) describes that
product split. The implementation is a Rust CLI that also hosts MCP and HTTP
services; installed agents, Git, external tools, and model providers remain
separate programs or services. The root library exposes only `code`, rather
than all the binary's modules. [Sources: CLI dispatch](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/mod.rs#L86),
[binary entry](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/main.rs#L96), [library facade](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/lib.rs).

## Product features and capabilities

This catalog describes **what a user or agent can accomplish**, separately from
the implementation map below. “Implemented” means a handler/command exists in
this snapshot; it does not mean an end-to-end run was tested here. CLI and MCP
coverage differ: a feature exposed by the `item` MCP tool is not necessarily an
`agentflare item` CLI subcommand. MCP names below are the server's base names;
clients can prefix them (for example, `mcp__flare__item`).
[CLI dispatch](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/mod.rs#L86), [MCP tool definitions](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server.rs#L1705).

### Set up agents and improve individual sessions

| Capability / user outcome | Access and source | Conditions or limits |
|---|---|---|
| Connect an existing coding-agent host to Agentflare hooks, rules and MCP | CLI `init`, `doctor`; [installation flow](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/init.rs#L92) | Host support is not uniform. Configuration can be merged/refreshed; restart the host after setup. |
| Detect, install, update, remove and launch agent CLIs; name sessions and assign teams | CLI `agents list/doctor/install/update/uninstall/launch`, plus `run`; [agent actions](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/agents.rs#L4) | External agent installations, their package managers and credentials remain prerequisites; Agentflare does not itself supply a coding model. |
| Compress a Markdown work product and recover a registered original later | CLI `optimize output compress`, `optimize retrieve`; [output command](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/optimize.rs#L9), [retrieval](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/optimize.rs#L519) | Compression uses an LLM. Omitted target means in-place output; the retrieval id refers to registered output-layer content, not arbitrary lean-ctx reads. |
| Apply code-minimalism, review, audit and related instruction playbooks | CLI `optimize code`; host hooks; [modes and previews](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/optimize.rs#L114) | Several subcommands **print instructions for an agent**; their names do not mean the binary independently performed an audit or measured savings. |
| Rank transcript lines by relevance and receive runtime/session nudges | CLI `optimize context score`; lifecycle hooks; [score command](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/optimize.rs#L529), [hook runtime](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/hook.rs#L504) | The compatibility `PreCompact` callback is inert. lean-ctx supplies separate tool-I/O/context functionality. |
| Add persistent coaching rules that appear at session start, on selected tools or when relevant to a prompt | CLI `coaching apply/list/remove/sync/enforce`; [actions](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/coaching.rs#L5) | Enforcement requires the host's supported PreToolUse path; advisory text and hard-denied rules have different behavior. |

### Remember work and find relevant information

| Capability / user outcome | Access and source | Conditions or limits |
|---|---|---|
| Preserve observations, decisions and session context across sessions; recall relevant notes | CLI `memory`; MCP `memory`; [memory modules](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/memory/mod.rs), [recall implementation](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/memory/search.rs#L106) | Local FTS is available without semantic embeddings; hybrid recall depends on the compiled feature and model availability. |
| Promote observations into a project lesson with execution receipts and explicit confidence | MCP `memory` promotion action; [validation](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/memory/promotion.rs#L23) | Requires live project-scoped evidence and a terminal run with recorded execution evidence. Provenance does not establish that the lesson is true. |
| Search local documents, memory and code, or query external information sources | MCP `search`; [actual dispatch](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server/search.rs#L5), [request controls](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server/types.rs#L1467) | `all` searches store+memory+code; `all+web` adds web. Other types include social, news, GitHub, academic, datasets, websites, weather, financial, crypto, FX, indicators, YouTube and Bluesky. Code search delegates to lean-ctx; external-source availability was not tested. |
| Retrieve/cache third-party package documentation, then search/list cached entries | CLI `docs`; MCP `flare_docs`; [lookup/cache flow](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server/flare_docs.rs#L25) | Cache misses can trigger network fetches. This cache is distinct from project documents and cross-session memory. |
| Discover, load, install and share reusable skills; recommend skills for a repository | CLI `skill`; MCP `skill` search/load and `skill_create`; [CLI actions](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/skill.rs#L14), [MCP requests](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server/types.rs#L67) | CLI includes registry management, import/export, hub push/pull, search evaluation and provisioning. Provisioning is a dry run unless `--yes` is supplied. MCP search/load is narrower than the CLI. |

### Plan, assign and execute a backlog

| Capability / user outcome | Access and source | Conditions or limits |
|---|---|---|
| Keep a shared backlog with projects, item states, assignees, labels, dates, parents, comments and typed dependencies | MCP `project`, `item`, `comment`, `label`; [item API](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server.rs#L1705), [request fields](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server/types.rs#L832) | Relations distinguish blocking dependencies from duplicates/related items. Projects can be archived to stop automatic dispatch/review while preserving readable data. [project behavior](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server.rs#L1918) |
| Groom a backlog, produce standup/health reports and inspect a multi-project portfolio | MCP `pm` and `item` reporting actions; [PM implementation](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server/pm.rs#L21) | Planning returns shortlist/Now/Next/Later buckets with a capacity parameter; business scoring such as RICE/WSJF remains the caller's job. This is not an autonomous product-prioritization engine. |
| Require a plan to be submitted and approved before implementation starts | MCP `item` actions `submit_plan`, `approve_plan`, `reject_plan`; [gate and transitions](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server/item.rs#L208), [submission](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server/item.rs#L1265) | Per-item plan policy must be configured; the existence of approval actions does not mean every item is gated. |
| Claim work without colliding with another session and provision an isolated worktree | MCP `item` claim/heartbeat/release; CLI `claim`; [claim implementation](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server/item.rs#L1068) | Ownership has a liveness/TTL lifecycle. Worktree isolation and process sandboxing are separate guarantees. |
| Run work through implement/analyze, review, judge and finalization roles; discover and dispatch eligible work in the background | CLI `work` for operator dispatch; daemon supervisor; [work entry](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/work.rs#L575), [pipeline](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/work_item_pipeline.rs#L482), [worker boot](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/dashboard/dispatch_boot.rs#L19) | Needs an installed/authenticated agent, eligible item and runnable project. The role loop has bounded fix/task counts. Automated results may require human follow-up. AI-agent callers are directed to item `redispatch` rather than recursively invoking `work`. [tool contract](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server.rs#L1705) |
| Inspect progress, pause/resume/cancel work, or re-arm a failed item | CLI `item`, `job`; MCP `item` status/control/redispatch; dashboard controls; [control implementation](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server/item.rs#L1967), [HTTP routes](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/dashboard/controls.rs#L73) | Pause retains resumable state/worktree; cancel is terminal. Item progress joins job, PR and daemon evidence rather than relying only on an agent's completion message. |
| Run a reusable JSON agent workflow, inspect its state/report, resolve a human wait, or cancel it | CLI `workflow`; MCP `workflow`; [tool contract](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server.rs#L1949), [JSON runner](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/workflow.rs#L449) | Workflow execution is distinct from work-item dispatch. Cancelling does not compensate already-succeeded steps. |
| Execute a project workflow repeatedly on an interval or enqueue a manual occurrence | CLI `workflow schedule`; MCP workflow schedule actions; [scheduler contract](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/schedule/mod.rs#L1) | Minimum interval 60 seconds. `fire` queues work; the daemon executes it. Disable/remove does not cancel an already-claimed run. |

### Review, collaborate and hand work off

| Capability / user outcome | Access and source | Conditions or limits |
|---|---|---|
| Collect review findings, verify cited changed lines, combine overlapping findings, and record acceptance-criterion verdicts | CLI `review`; MCP `review`; [consensus core](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/review.rs#L1), [request actions](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server/types.rs#L183) | Citation verification/consensus does not prove every finding correct or that a patch has no bugs. Criteria without evidence cannot retain a pass verdict. |
| Create/manage PRs, issues, workflow runs and releases; preserve cross-instance PR ownership | CLI `git`, `pr`, `github-bridge`; MCP `flare_git`; [GitHub action schema](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server/types.rs#L620), [supervisor](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/supervisor.rs) | GitHub credentials/policy are required. Supervisor discovery and review/repair are background policy paths, not a guarantee that a PR will be merged. |
| Send messages to sessions, teams, an item's worker, an agent kind or all live sessions | CLI `message`; MCP `message`; dashboard messages; [addressing and delivery contract](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server.rs#L1719) | Delivery timing varies by marker and host hooks/MCP activity. Peer messages are not instructions from the user. |
| Pass a work product/context to another runtime and retain threaded handoff context | CLI `handoff`; MCP `handoff`; [handoff request](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server/types.rs#L248) | The receiving runtime and delivery path must be available; handoff storage is separate from executing a new agent turn. |
| Publish versioned work products, compare versions, and attach files to items/projects | MCP `artifact`, `asset`; CLI `artifacts` for serving; [artifact actions/types](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server/types.rs#L374), [asset entry](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server.rs#L1932) | Asset attachment requires staging first. Artifact serving does not imply a public cloud deployment or authenticated sharing. |
| Capture workflow friction and notify external channels | CLI `vent`, `channel`; MCP vent/channel tools; [application modules](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/main.rs#L19), [MCP request types](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server/types.rs#L173) | Channel delivery requires configured destinations/credentials; external delivery and auto-filing outcomes were not exercised here. |

### Inspect sessions, operate tools and extend workflows

| Capability / user outcome | Access and source | Conditions or limits |
|---|---|---|
| View token/cost summaries and inspect/search/export historical agent sessions | CLI `cost`, `insights`; [insights actions](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/insights.rs#L14) | Insights can sync/watch sources, show turns/tool calls, report statistics, export and generate handoff documents. Implemented ingest coverage is Claude/Codex/OpenCode; support for other installed hosts should not be inferred. [adapter scope](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/flare-insights/src/lib.rs#L6) |
| Monitor boards, items, claims, jobs, messages and execution controls in a browser | CLI `serve`, `daemon`; [dashboard routes](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/dashboard/server.rs#L690) | Starting the dashboard can execute work. It is a local execution service with no general HTTP authentication, not just a reporting page. |
| Automate a browser using accessibility snapshots, element refs, actions, screenshots, PDFs and page reads | CLI `browser`; MCP `browser`; [command layer](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/browser.rs#L1) | Needs the external browser backend/browser installation. Session identity defaults to the working directory; first use can download dependencies. |
| Discover a downstream MCP tool and invoke it through one gateway | MCP `tool` search/execute; [request schema](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server/types.rs#L112), [gateway backends](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/gateway-registry/src/lib.rs#L17) | Requires configured/reachable downstream servers and their authentication. This is tool discovery/routing, not a provider-independent guarantee about results. |
| Manage saved agent-auth profiles, rotations/cooldowns and project associations; store project secrets | CLI `auth`, `vault`; [auth actions](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/auth.rs#L4), [vault actions](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/vault.rs#L4) | Auth profiles and the secret vault are separate mechanisms. Provider quotas and login validity are external; clearing a local profile does not revoke a remote credential. |
| Inspect change impact and maintain worktrees/branches | CLI `code impact`, `git`, `clean`; [impact implementation](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/code/mod.rs#L45), [Git primitives](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/flare-git-core/src/lib.rs#L1) | The impact implementation uses a Cargo workspace graph and reference checks; it is not a general whole-program analyzer for every language. Cleanup can mutate files/worktrees and must be reviewed. |
| Route Anthropic-style requests to configured alternate model providers | Dashboard `/proxy/v1/messages`; [proxy router](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/flare-proxy/src/lib.rs#L18) | Needs upstream configuration/credentials; proxy token enforcement is conditional on configuration. Provider/model compatibility is not universally guaranteed. |
| Run an app packaged with its own manifest, tools and domain workflow | CLI `apps run`; [app entry](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/apps.rs#L21), [app sender](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/workflow.rs#L276) | Existing surface runs an app directory. It does not establish a hosted app marketplace or independent multi-tenant app service. |

### Product journeys and maturity

The capabilities compose into four main user journeys:

1. **Improve an individual session:** initialize a supported host, choose code
   instructions/coaching, recall prior memory, then use search/docs/skills as
   needed. Hook support and optional tooling determine the available experience.
2. **Deliver work from a shared backlog:** create and groom an item, approve its
   plan when required, dispatch it into a worktree, inspect/review the result,
   then confirm its PR merged before considering the work completed.
3. **Run a repeatable process:** define a project JSON workflow, run it directly
   or schedule it, resolve human waits, and inspect the durable execution report.
4. **Continue work across agents:** inspect the prior session, publish a durable
   artifact, hand off context or message the next session, and retain lessons
   with explicit evidence rather than promoting completion claims automatically.

These are compositions of the implemented paths above, not additional product
features. [STATUS.md](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/STATUS.md) describes optimization/memory/setup as more
settled and coordination/MCP formats as actively changing. **No blanket stability
rating or performance improvement is inferred from a feature's presence.**
This pass inspected handlers and successfully ran `--help` for agents, item, job,
work, review, skill, insights, apps, coaching, auth, vault, docs and pr; it did
not execute those workflows or contact their external services.

## Detected stack

| Layer | Technology | Evidence |
|---|---|---|
| Application | Rust 2024; package version 1.8.1; declared minimum Rust 1.91 | [Cargo.toml](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/Cargo.toml#L8) |
| Development toolchain | Rust 1.98.0, clippy, rustfmt | [rust-toolchain.toml](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/rust-toolchain.toml#L1) |
| CLI / serialization | clap 4 derive, serde, JSON, TOML | [Cargo.toml](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/Cargo.toml#L46) |
| Async / HTTP / MCP | Tokio 1, Axum 0.8, rmcp 1.8.0 with stdio and streamable HTTP | [Cargo.toml](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/Cargo.toml#L53), [MCP server](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server.rs#L2184) |
| Persistence | Bundled SQLite through rusqlite 0.40; FTS5; per-subsystem migrations | [Cargo.toml](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/Cargo.toml#L66), [DB helper](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-db-kit/src/open.rs#L54), [memory search](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/memory/search.rs#L74) |
| Optional local embeddings | `semantic` feature forwards to store embeddings; ONNX model downloaded on first use | [Cargo.toml](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/Cargo.toml#L123), [memory engine](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/memory/engine.rs) |
| Dashboard | Embedded static assets, Axum JSON routes, server-sent events | [dashboard server](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/dashboard/server.rs#L1), [routes](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/dashboard/server.rs#L690) |
| Public website | Cloudflare Worker and static assets; Wrangler 4 | [site/package.json](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/site/package.json), [wrangler.jsonc](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/site/wrangler.jsonc#L1) |
| Documentation site | Astro 7.1.3 range, Starlight 0.41.4 range; mounted at `/docs` | [docs manifest](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/docs-site/package.json), [Astro config](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/docs-site/astro.config.mjs#L9) |
| JS task tooling | Two-member aube workspace; mise tasks | [aube-workspace.yaml](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/aube-workspace.yaml#L1), [mise.toml](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/mise.toml) |

`cargo metadata --offline --locked --no-deps --format-version 1` reported
**32 workspace packages**: the root package plus 31 explicitly listed members.
`flare-db` and `flare-db-macros` are deliberately excluded, with independent
database feature matrices and release lockfiles. Older documentation's
“28-member workspace” count was stale. [Source](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/Cargo.toml#L1).

## Entry points and process ownership

| Entry | Implementation and behavior |
|---|---|
| `agentflare <command>` | `main` installs error reporting, initializes mise PATH, parses clap, then dispatches through `Commands::run`. With no command it prints the about view. [main](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/main.rs#L96), [dispatch](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/mod.rs#L192) |
| `agentflare hook … --agent …` | Short-lived host lifecycle handlers: session start/end, prompt, tool calls, failures, completion. Host wiring differs. [hook handlers](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/hook.rs), [Codex installation](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/init.rs#L644) |
| `agentflare mcp` | Stdio MCP server, ordinarily one process per client. It does not require the dashboard daemon. [implementation](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server.rs#L2184) |
| `agentflare mcp --http` | Persistent shared `AgentflareMcp`, streamable HTTP at `/mcp`; default `127.0.0.1:35274`. No authentication is implemented here; the CLI documents the default Host allowlist. [options](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/mcp.rs#L6), [server](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mcp_server.rs#L2193) |
| `agentflare serve` / `agentflare daemon start` | Dashboard and execution host. Default `127.0.0.1:35273`; embedded assets, REST/SSE, worker pool, recovery, scheduler, supervisor, and proxy route. [serve](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/serve.rs#L5), [boot](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/dashboard/server.rs#L770) |
| `site/src/worker.ts` | Separately deployed public website, serving `site/public`; docs are copied into those assets at deploy time. [Worker config](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/site/wrangler.jsonc#L1), [deploy scripts](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/site/package.json) |

**Resolved contradiction — the dashboard is not read-only.** CLI help still
calls it read-only, but its router accepts `POST /api/jobs`, messages, chat,
item/job controls, and gate reset. Startup can execute queued work and scheduled
workflows. Treat `serve` as starting the execution service, including when it
is launched only to view the UI. Non-loopback binding requires `--yes-expose`;
this is acknowledgement, not access control. [Routes](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/dashboard/server.rs#L690),
[controls](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/dashboard/controls.rs#L73), [messages](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/dashboard/messages.rs#L30),
[exposure check and startup](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/dashboard/server.rs#L770).

---

Next: [Installation and verification](/docs/reference/architecture/installation-and-verification/) | [Internals](/docs/reference/architecture/internals/)

