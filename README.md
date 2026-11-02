<div align="center">

<pre>
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  agentflare  ·  Optimize AI CLI agents for cost & performance
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
</pre>

# agentflare

**Run AI coding agents efficiently, and coordinate more than one of them.**
**A Rust binary for Claude Code, Codex, Cursor, Windsurf, VS Code, Cline,
and Continue. The core CLI needs no Node; integrations and optional capabilities
use external tools.**

[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)
[![Status: Beta](https://img.shields.io/badge/status-beta-yellow.svg)](STATUS.md)

</div>

---

## Status: Beta

agentflare is under active development. The optimization layer (lean-ctx
integration, memory, `optimize output`/`code`/`context` and the always-on
runtime layer) is the most mature part — CI-gated, tested, in daily use. The
multi-agent coordination layer (tasks, review, coaching, artifacts, handoffs,
daemon, `work`) is newer and still finding its shape: CLI flags, MCP tool
names, and on-disk formats there can change without a major version bump.

See **[STATUS.md](STATUS.md)** for what's stable vs. still moving, before you
build automation on top of a specific flag or MCP tool signature.

## What this is

agentflare is two things bundled into one binary:

**1. Optimization** — cut the token/cost overhead of running a single AI coding
agent session.

| Layer | What it compresses | Tool |
|---|---|---|
| **lean-ctx** | focused views of tool I/O *within* a session — reads, shell output and search, with references for recovering detail | [yvgude/lean-ctx](https://github.com/yvgude/lean-ctx) |
| **memory** (built-in) | knowledge *across* sessions — decisions, facts, preferences that survive a session ending | ships in the binary, SQLite + FTS5, no separate install |
| **`agentflare optimize output`** (formerly Caveman) | compresses markdown files you give it, so the agent reads fewer tokens each time it loads them (needs model access) | built into the binary |
| **`agentflare optimize code`** (formerly Ponytail) | code-writing minimalism, hooked into Claude Code and Codex | built into the binary |
| **`agentflare optimize context`** | on-demand BM25/FTS5 relevance scoring over a session transcript (`optimize context score`). The `PreCompact` hook is wired for upgrade compatibility but inert: Claude Code's `PreCompact` accepts no injected context, so compaction survival is left to lean-ctx | built into the binary |
| **`agentflare optimize retrieve`** | reversible-compression retrieve (CCR) — pulls back the original, full-fidelity content that output/context compression replaced, when an agent actually needs it | built into the binary |
| **runtime layer** (always-on, no CLI surface) | automatic session hygiene and model-routing nudges, surfaced via hooks | built into the binary |

`agentflare optimize` also answers to its legacy alias `agentflare flare` (and `opt`) for
backward compat with earlier install scripts.

**2. Coordination** — a lightweight, local-first backend for running *more than
one* agent (or agent session) against the same body of work, exposed as MCP
tools any MCP-capable agent can call:

| Capability | What it's for |
|---|---|
| **Work items** (`item`, `claim`) | A shared backlog — create/list/search items, claim one before working it so two agents don't collide, mark done. |
| **Autonomous work** (`work`) | Claim an item, provision a worktree, run a headless agent against it, and report back (comment + PR) — unattended, end to end. |
| **Review** (`review`) | Submit findings against a diff/PR; agentflare verifies each citation against the actual diff, dedups overlapping findings, and tags each CONFIRMED/UNIQUE/DISPUTED/UNVERIFIED. |
| **Artifacts** (`artifact`) | Publish specs, plans, and docs as versioned, shareable pages — a durable handoff surface between agents (and to you) instead of scratch files that vanish with the session. |
| **Handoff** (`handoff`) | Pass context to a specific agent/runtime, addressed and threaded, when work moves from one agent to another. |
| **Coaching** (`coaching` CLI + hook) | Small persistent nudges surfaced to an agent — at session start, and via contextual triggers (BM25 relevance against the tool call/prompt) so a rule only shows up when it's actually relevant. |
| **Daemon** (`daemon`) | Background lifecycle for the coordination layer — PID file, flock lock, Unix socket/Windows named-pipe IPC, launchd/systemd autostart, cached update checks. |
| **Dashboard** (`serve`) | Web dashboard and execution host — boards, items, cost, claims, job submission and controls, workflow recovery, workers and schedules. |
| **Friction logging** (`vent`) | Append-only per-repo JSONL of tool/workflow friction as it happens; a classifier auto-files the actionable entries as backlog items. |
| **GitHub ops** (`flare_git` MCP tool, `git` CLI) | PRs, issues, releases, and workflow runs against GitHub — including a bounded `pr_wait` poll — on top of the branch-protection/provenance git shim. |
| **Skills** (`skill`) | Registry for `SKILL.md` playbooks — search (BM25), load, and eval, so an agent can pull in a project-specific procedure on demand. |
| **Auth vault** (`auth`, `vault`) | Encrypted credential storage for agent auth profiles (rotation, cooldown, health scoring); `vault` separately holds lightweight per-project secrets (unlock/lock/print-env). |
| **Global search** (`search` MCP tool) | One entrypoint across 17 sources — store, memory, code, web, social, news, GitHub, academic, datasets, websites, weather, financial, crypto, fx, indicators, YouTube, Bluesky. |
| **`@mention` references** | Inline `@I…`/`@A…`/`@search:…` references in tool output resolve across items, artifacts, and search results. |
| **Git-aware PATH shim** | Impersonates `git`; branch-protection policy resolves against the *target file's own repo*, not the host process's cwd, so a stray write can't land on `master`/`main` from the wrong worktree. |
| **Comments, labels, projects, webhooks, channel_send** | Threaded discussion on items, categorization, cross-project views, and outbound notifications (Telegram/Slack/Discord). |

See [Git shim workflow](GIT-WORKFLOW.md) for worktree claims, protected-branch
checkout, scope checks, and approved force-push retries.

A companion crate, `flare-proxy`, provides an Anthropic-compatible HTTP proxy in
front of alternate/free OpenAI-style providers. The dashboard mounts it at
`/proxy/v1/messages`; provider routing is configured separately from agent setup.

The coordination stores are local-first. Stdio MCP can access them without a
daemon. The optional `daemon` / `serve` process hosts background execution:
workers, workflow recovery, supervisor ticks and interval schedules. Starting
`serve` can execute queued work; its HTTP surface includes write/control routes
and has no general authentication. Keep its default loopback binding.
See the [source-backed architecture and operator reference](ARCHITECTURE.md)
for process boundaries, installation differences and verification limits.
For user outcomes, entry points and prerequisites, see the
[product feature and capability catalog](ARCHITECTURE.md#product-features-and-capabilities).

lean-ctx and the built-in memory aren't substitutes for each other — one saves
tokens inside a session, the other saves the re-explaining tax across sessions.
The coordination layer is a different axis entirely: it's not about a single
session's token bill, it's about multiple agents (or multiple sessions of the
same agent, over time) staying out of each other's way and handing off work
cleanly.

## Evidence-backed project lessons

Raw observations stay observations until an explicit `memory` MCP promotion:

```json
{"action":"promote","title":"Prefer the bounded launch path","content":"Why this applies here","project":"my-project","evidence_ids":[12],"confidence":0.8,"run_id":"<workflow-run-uuid>"}
```

The run must exist in the workflow database, be terminal, contain a recorded
execution outcome, and belong to a work item in the requested project. Supporting
observations must be live and project-scoped. Promotion records the run/journal
link and observation hashes in the existing relations table. Execution provenance
does not prove a lesson correct; confidence remains an explicit estimate.

Recall, context, memory search and observation listings include lesson provenance
and the minimum confidence across its evidence receipts. Lesson contents and
receipts are immutable: create a new lesson for a revision, optionally relating it
with `supersedes`. Ordinary remember/curate writes cannot create or rewrite lessons.
Memory sync excludes local lessons and skips remote lesson imports while preserving
remote records, because its format does not carry locally verified run receipts.
Generic workflows without work-item project provenance cannot back promotion yet.

## Workflow schedules

`agentflare workflow schedule` registers an interval trigger for a project-local
workflow at `<repo>/.agentflare/workflows/<name>.json`. Schedules and their
occurrence history live in `~/.agentflare/workflows.db` (UTC epoch seconds,
`every` at least 60s). The same actions are on the `workflow` MCP tool:
`schedule_create`, `schedule_list`, `schedule_show`, `schedule_update`,
`schedule_enable`, `schedule_disable`, `schedule_remove`, `schedule_fire`,
`schedule_runs`.

Manual `fire` (CLI or `schedule_fire`) only enqueues a durable occurrence and
returns its id with status `queued`. It does not start a workflow run. The
daemon claims that queue on its next tick — every 60 seconds, including the
first tick after boot — and is the only process that executes it. Exiting or
killing the client does not abandon a queued occurrence. While the daemon is
down, fire still succeeds and reports `queued; waiting for daemon`.

The schedule ticker starts next to the binary-staleness watchdog, outside
work-item dispatch recovery. A failed work-item smoke test, registration, or
recovery turns work dispatch off only; schedules keep firing.

`disable` stops new scheduled admissions. It does not cancel a run the daemon
already claimed, and a manual fire of a disabled schedule still enqueues.
`remove` deletes the schedule and its occurrence history. It does not cancel
a workflow that has already started. Start receipts in `workflow_start_receipts`
stay behind so a later tick cannot launch that occurrence again.

The driver writes the terminal outcome into `schedule_runs` when it finishes.
The next tick copies the outcome if that write failed. A paused run stays
`dispatched` and keeps blocking overlap until something resumes or cancels it.
A committed start whose driver never launched is recorded as `interrupted`
and is not replaced with a new run.

Each schedule keeps the newest 100 non-skip terminal occurrences and,
separately, the newest 100 skip rows (`skipped_missed`, `skipped_overlap`).
Rows that are still queued, dispatching, or dispatched are never pruned to
make room. Receipts are kept for the life of the database in this phase.

**Why Rust:** the core CLI and hooks run as a native binary without requiring
Node. That does not bundle every integration: lean-ctx is separate, agent
execution needs the selected agent and credentials, and browser automation can
download mise, agent-browser and Chrome for Testing on first use. See
[installation and first use](ARCHITECTURE.md#installation-and-first-use).

**No plugin marketplace required for Claude Code, Codex, or Cursor** —
`agentflare init --agent X` writes hooks into the target's own settings file
(`~/.claude/settings.json`, `~/.codex/hooks.json`, or `.cursor/hooks.json`).
For Codex, use init or the plugin, not both: installing both duplicates lifecycle hooks.

## Metrics

The result below comes from the upstream project's benchmark, which agentflare has
not re-run. It is not a prediction for your project. The coordination layer has no
comparable benchmark suite yet (see [STATUS.md](STATUS.md)).

| Tool | Reported result | Source and method | Limits |
|---|---|---|---|
| `agentflare optimize code` | About 54% less code, 20% lower cost and 27% faster | [Ponytail's agentic benchmark](https://github.com/DietrichGebert/ponytail/blob/main/benchmarks/results/2026-06-18-agentic.md): 12 feature tasks on one FastAPI + React repository, Haiku 4.5, four runs per task/arm | One repository and model, small sample. Code volume does not measure code quality. |

lean-ctx's earlier read-mode figures are [withdrawn by upstream](https://github.com/yvgude/lean-ctx/blob/main/BENCHMARKS.md).
Measure your repository with `lean-ctx benchmark report .`; local representation
comparisons do not establish model quality or monetary savings.

### Real usage, one live project

Not a demo — pulled live from the maintainer's own project while building this repo,
for a sense of scale. Not a controlled benchmark; one data point, your mileage varies.

```text
optimize code    code-minimalism markers logged, no token figure        (agentflare optimize code doesn't measure per-repo savings)
memory           2 sessions, 11 observations tracked, across 2 projects (agentflare memory sessions/search)
```

Check your own: `lean-ctx gain` · `agentflare cost` · `agentflare optimize code gain`/`debt` ·
`agentflare memory context`. Don't trust this table blindly either — re-run those commands yourself.

---

## Install the CLI

**Linux/macOS** (downloads a prebuilt binary; verifies its checksum when
`sha256sum` or `shasum` is available, otherwise warns and skips verification;
builds from source instead if run from inside a clone):
```bash
curl -fsSL https://raw.githubusercontent.com/getappz/agentflare/master/install.sh | sh
```

**Homebrew:**
```bash
brew tap getappz/agentflare
brew install agentflare
```

**Windows, build from source** (requires Rust and a working native linker):
```powershell
git clone https://github.com/getappz/agentflare
cd agentflare
.\install.ps1
```

**Windows, Scoop** (prebuilt binary — not Authenticode-signed, so
Defender/SmartScreen false-positives are possible; verify with cosign/SLSA
instead, see "Verifying release binaries" below; report an issue if hit):
```powershell
scoop bucket add agentflare https://github.com/getappz/agentflare
scoop install agentflare
```

**Any platform with Rust, no clone needed:**
```bash
cargo install --git https://github.com/getappz/agentflare
```

**Uninstall:**
```bash
curl -fsSL https://raw.githubusercontent.com/getappz/agentflare/master/install.sh | sh -s -- --uninstall
```

---

## Verifying release binaries

The Unix download installer verifies SHA-256 checksums when a checksum tool is
available; source builds do not download a release binary. Checksums can
catch a corrupted download, not a substituted one. For higher-assurance
environments, verify the cryptographic signature and build provenance before
running the binary.

### cosign (signing identity)

Every release binary is signed in CI using
[cosign](https://docs.sigstore.dev/cosign/overview/) keyless signing via the
GitHub OIDC token — the certificate is issued by Fulcio and bound to this
repo's `release.yml` workflow, so verifiers pin to the workflow identity
instead of a long-lived key.

```bash
VERSION=v0.x.x
FILE=agentflare-x86_64-unknown-linux-gnu.tar.gz
curl -fL -o "$FILE" "https://github.com/getappz/agentflare/releases/download/${VERSION}/${FILE}"
curl -fL -o "${FILE}.cosign.bundle" "https://github.com/getappz/agentflare/releases/download/${VERSION}/${FILE}.cosign.bundle"

cosign verify-blob \
  --bundle "${FILE}.cosign.bundle" \
  --certificate-identity-regexp '^https://github\.com/getappz/agentflare/\.github/workflows/release\.yml@refs/tags/v.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  "$FILE"
```

### SLSA L3 provenance (build process)

cosign proves *this repo's CI signed it*; SLSA provenance proves *how it was
built* — which commit, workflow, and inputs. Releases include a
`<tag>.intoto.jsonl` attestation generated by the
[SLSA GitHub generator](https://github.com/slsa-framework/slsa-github-generator).
Verify with [`slsa-verifier`](https://github.com/slsa-framework/slsa-verifier):

```bash
curl -fL -o "${VERSION}.intoto.jsonl" "https://github.com/getappz/agentflare/releases/download/${VERSION}/${VERSION}.intoto.jsonl"

slsa-verifier verify-artifact \
  --provenance-path "${VERSION}.intoto.jsonl" \
  --source-uri github.com/getappz/agentflare \
  --source-tag "${VERSION}" \
  "$FILE"
```

Both print a `Verified`/`PASSED` line and exit 0 on success — do not run the
binary on failure. `--certificate-identity-regexp`/`--certificate-oidc-issuer`
and `--source-uri`/`--source-tag` are the load-bearing flags in each command;
loosening any of them defeats the point.

| Attack | SHA-256 checksums | cosign keyless | SLSA L3 provenance |
|---|---|---|---|
| Corrupted download | ✅ caught | ✅ caught | ✅ caught |
| Substituted binary at release | ❌ SHA256SUMS would also be swapped | ✅ certificate identity ≠ this repo's workflow | ✅ provenance source-uri ≠ this repo |
| Stolen release-pipeline secret | ❌ | ✅ no long-lived secret to steal | ✅ provenance binds to specific workflow run |
| Tampered build process | ❌ | ❌ — cosign signs the artifact, not the build | ✅ provenance records the exact workflow, commit, and inputs |

SHA-256 stays the default in the installers above because it needs no extra
client-side tooling; cosign and SLSA are opt-in for environments that need
the higher tier.

## Set up an agent

One command per tool. `init` checks existing components, installs missing ones,
merges host configuration, and can prompt to refresh rules or configure optional
integrations.

```bash
agentflare init --agent claude-code    # writes ~/.claude/settings.json hooks directly, no marketplace
agentflare init --agent codex          # writes ~/.codex/hooks.json and registers the MCP server
agentflare init --agent cursor         # writes .cursor/hooks.json directly, no marketplace
agentflare init --agent windsurf
agentflare init --agent vscode-copilot
agentflare init --agent cline
agentflare init --agent continue
```

Codex loads hooks from `~/.codex/hooks.json`; `init` wires the supported
agentflare lifecycle hooks there. The `.codex-plugin/` compatibility manifest
also supports packaging those hooks in a Codex plugin. Use one hook installation
path per machine so events do not run twice. In Codex, review the installed
hooks with `/hooks` and trust them before relying on them in interactive or
headless runs. Codex requires trust for non-managed hooks. After reviewing a
work item, `mcp__flare__review(action="submit", findings=[...])` records review
completion for the item gate; an empty findings list is valid when the review
found no issues. For verification, run tests through `mcp__lean_ctx__ctx_shell`:
Codex's native Bash hook omits the process exit status, so its result cannot
prove that a test passed.

Each run checks rule files, installs lean-ctx if missing, and wires hooks/MCP
where the host supports it. Already-satisfied components are skipped; managed
rules and existing hook configuration may be refreshed or merged. Persistent
memory ships in the binary itself — nothing to install for it.

## Inter-agent messaging

Live sessions (Claude Code, Codex, Cursor, daemon-dispatched jobs) and humans
exchange short messages through `agentflare.db`. Send from the MCP `message`
tool or the CLI; delivery lands in the recipient's context through its hooks
(or piggybacked on the next agentflare tool result on hosts without hooks).

```bash
agentflare message send <to> "text" [--marker important|status|fyi] [--reply-to <id>]
agentflare message list                 # live sessions: key, name, item, team, busy
agentflare message inbox                # your unread mail
agentflare message history --to team:alpha [--after <id>] [--limit N]
agentflare message watch                # stream your mailbox, one line each
```

Addresses: a session key or unique name (`message list`), `item:<id>`
(whoever works that item), `agent:<name>` (every live session of that agent),
`team:<name>` (every live session launched with `--team <name>`, except you),
or `*` (everyone).

Teams: `agentflare agents launch <agent> --team alpha --name pm` (or
`agentflare run <agent> --team alpha --name pm`) registers the session under
`team:alpha` and names it `pm`. A session that joins a team gets the last 10
`team:alpha` messages replayed at SessionStart, marked as history.

Markers decide *when* a message is pushed, never whether it is stored:

| Marker | SessionStart / PromptSubmit | PreToolUse (mid-turn) | Stop | inbox / watch / MCP piggyback |
|---|---|---|---|---|
| `important` (default) | deliver | deliver | block stop and deliver | yes |
| `status` | deliver | only once 3 or more are pending | block stop and deliver | yes |
| `fyi` | deliver | never | never blocks the stop | yes |

Codex runs sandboxed: let it write the message store by adding to
`~/.codex/config.toml`:

```toml
[sandbox_workspace_write]
writable_roots = ["~/.agentflare"]
```

## Docs-only fallback (Aider, other AGENTS.md readers)

```bash
curl -sL https://raw.githubusercontent.com/getappz/agentflare/master/AGENTS.md > AGENTS.md
```

---

## Architecture

The comprehensive [architecture and operator reference](ARCHITECTURE.md)
documents the inspected source snapshot, major subsystems, installation,
command verification, storage, recovery and platform limitations with citations.

```
src/
├── main.rs                  # clap CLI entrypoint, dispatch across the modules below
├── cli/                     # one file per top-level subcommand (init, hook, optimize, work,
│                             # vent, auth, vault, git, review, skill, memory, ...) — thin clap
│                             # wiring; the real logic lives in the modules it calls into
├── mcp_server/               # one file per mcp__flare__* MCP tool (item, artifact, asset,
│                             # claim, review, handoff, memory_tool, flare_git, flare_docs,
│                             # search, skill, comment, builtin_tools, types) + tests/
├── optimize/                  # the optimization layer:
│                             #   output.rs   — agentflare optimize output (prose compression)
│                             #   code.rs     — agentflare optimize code (code minimalism)
│                             #   context.rs  — BM25/FTS5 session-transcript compaction
│                             #   retrieve.rs — reversible-compression retrieve (CCR)
│                             #   runtime.rs  — always-on session-hygiene/model-routing nudges
├── coaching/                  # session nudges: rule storage, CRUD, session-start +
│                             # contextual BM25-triggered presentation
├── memory/                    # built-in persistent memory (SQLite + FTS5): embeddings,
│                             # search, sessions, relations, summaries
├── github/                     # GitHub API client backing `flare_git`/`git` — actions,
│                             # issues, pulls, releases, identity, auth
├── dashboard/                   # `agentflare serve` dashboard and execution host backend
│                             # (pairs with the top-level dashboard/web static frontend)
├── vent/                         # friction capture, auto-classification into backlog items
├── mentions/                      # @mention inline reference parsing/resolution (@I/@A/@search)
├── ipc/                            # daemon transport — Unix socket / Windows named pipe
├── dev_install/                     # `agentflare dev-install` — build the current checkout,
│                                   # install over the running binary
├── update/                           # self-update check + binary swap
├── ui/                                # terminal UI helpers (spinner, cliclack prompts)
├── core/                                # small shared primitives (codesigning helpers)
├── rule_text.rs              # shared rule copy (Exa, git, lean-ctx usage) — paths.rs/state.rs
│                           # moved to crates/agentflare-bin-lib
├── compact.rs                 # FTS5/BM25 scorer still used by coaching;
│                             # the compatibility PreCompact hook is a no-op
├── claims.rs, review.rs        # work-item claiming, review/consensus core logic
├── artifacts.rs, channels.rs     # artifact publishing, outbound notifications core logic
├── auth.rs, auth_crypt.rs,
│   auth_db.rs, auth_runner.rs      # auth-profile vault (credential rotation/health scoring)
├── vault.rs                          # lightweight per-project secrets vault (unlock/lock/env)
├── daemon.rs, daemon_autostart.rs,
│   daemon_client.rs                   # daemon lifecycle: PID file, flock, autostart, IPC client
├── mcp_server.rs, mcp_prompts.rs        # MCP stdio server wiring, exposes mcp_server/* + native
│                                       # Claude Code slash commands for `/optimize*`
├── components.rs                         # registry: each entry checks + fixes itself, host-aware
├── init.rs                                # `agentflare init --agent X` — runs every component,
│                                         # wires hooks directly for claude-code/cursor
└── hook.rs                                 # `agentflare hook session-start|prompt-submit|... --agent X`

crates/                     # 32 packages including the root and agentflare-workspace-hack:
                            # flare-code, flare-output, flare-search-kit,
                            # agentflare-store, agentflare-backend, agentflare-db-kit, agent-registry,
                            # skill-registry, gateway-registry, flare-docs, flare-git-core,
                            # flare-git-shim, agentflare-shim, agentflare-artifacts, agentflare-jobs,
                            # flare-proxy, flare-vault, agentflare-bin-lib, ...
dashboard/web/               # static frontend served by `agentflare serve`
.codex-plugin/              # optional Codex plugin compatibility manifest
hooks/                      # Codex plugin lifecycle hooks
install.sh, install.ps1      # installers (checksum-verified download / local build)
.github/workflows/          # ci.yml (build+test), release.yml (cross-compile on tag)
```

For a new managed component, start with its check/apply entry in `components.rs`.
New host support can also require explicit installation and hook wiring in
`init.rs` and the hook handlers.

---

## What Gets Created

**Claude Code**: `~/.claude/rules/{exa,git,lean-ctx}.md`, `~/.claude/settings.json` hooks section, `~/.agentflare/` (includes the built-in memory database and the optimize layer's own state — no more separate `~/.config/{caveman,ponytail}/` now that both are built into the binary).

**Codex**: project-local `AGENTS.md` (only if absent), `~/.codex/hooks.json`,
MCP registration, `~/.agentflare/`.

**Cursor**: project-local `.cursor/rules/agentflare.mdc`, `.cursor/hooks.json`, `~/.cursor/mcp.json`, `~/.agentflare/`.

**Windsurf/VS Code/Cline**: project-local rules file (see table above), MCP config for lean-ctx.

**Continue**: `.continue/mcpServers/agentflare.json`.

Already-satisfied components are skipped; existing configuration can be merged
and managed rules refreshed. See [the integration lifecycle](ARCHITECTURE.md#deep-dive-1-host-integration-and-hook-policy).

---

## Uninstall

Remove the binary (see Install section above), then remove whatever `init`
wrote for the hosts you set up — see "What Gets Created" above. `optimize
output`/`optimize code` are part of the same binary now — no longer separate
plugins — so there's nothing left to uninstall separately for them. If you
have a config left over from before they were absorbed, `agentflare uninstall`
also cleans up the legacy `~/.config/{caveman,ponytail}/config.json`.

---

<div align="center">

Apache License 2.0

</div>
