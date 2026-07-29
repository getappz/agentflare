<div align="center">

<pre>
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  agentflare  ·  Optimize AI CLI agents for cost & performance
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
</pre>

# agentflare

**Run AI coding agents efficiently, and coordinate more than one of them.**
**A single Rust binary, no Node, no runtime dependencies — across Claude Code,
Codex, Cursor, Windsurf, VS Code, Cline, and Continue.**

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
| **lean-ctx** | tool I/O *within* a session — reads, shell output, search, up to 99% | [yvgude/lean-ctx](https://github.com/yvgude/lean-ctx) |
| **memory** (built-in) | knowledge *across* sessions — decisions, facts, preferences that survive a session ending | ships in the binary, SQLite + FTS5, no separate install |
| **`agentflare optimize output`** (formerly Caveman, Claude Code only) | conversation verbosity, ~65% | built into the binary |
| **`agentflare optimize code`** (formerly Ponytail, Claude Code only) | code-writing over-engineering | built into the binary |
| **`agentflare optimize context`** | session-transcript compaction — BM25/FTS5 relevance scoring on `PreCompact`, so a context compaction keeps what's relevant | built into the binary |
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
| **Dashboard** (`serve`) | Read-only web dashboard over the same local-first store — boards, items, cost, claims — for a glance without an MCP client. |
| **Friction logging** (`vent`) | Append-only per-repo JSONL of tool/workflow friction as it happens; a classifier auto-files the actionable entries as backlog items. |
| **GitHub ops** (`flare_git` MCP tool, `git` CLI) | PRs, issues, releases, and workflow runs against GitHub — including a bounded `pr_wait` poll — on top of the branch-protection/provenance git shim. |
| **Skills** (`skill`) | Registry for `SKILL.md` playbooks — search (BM25), load, and eval, so an agent can pull in a project-specific procedure on demand. |
| **Auth vault** (`auth`, `vault`) | Encrypted credential storage for agent auth profiles (rotation, cooldown, health scoring); `vault` separately holds lightweight per-project secrets (unlock/lock/print-env). |
| **Global search** (`search` MCP tool) | One entrypoint across 17 sources — store, memory, code, web, social, news, GitHub, academic, datasets, websites, weather, financial, crypto, fx, indicators, YouTube, Bluesky. |
| **`@mention` references** | Inline `@I…`/`@A…`/`@search:…` references in tool output resolve across items, artifacts, and search results. |
| **Git-aware PATH shim** | Impersonates `git`; branch-protection policy resolves against the *target file's own repo*, not the host process's cwd, so a stray write can't land on `master`/`main` from the wrong worktree. |
| **Comments, labels, projects, webhooks, channel_send** | Threaded discussion on items, categorization, cross-project views, and outbound notifications (Telegram/Slack/Discord). |

A companion crate, `flare-proxy`, runs an Anthropic-compatible HTTP proxy in
front of alternate/free OpenAI-style providers — useful for routing an agent
through a different backend without changing its client code; it isn't wired
into `agentflare init` yet.

Everything above is local-first (SQLite-backed) and reachable over the same
stdio MCP transport agentflare already exposes for the optimization layer, with
no daemon required. The `daemon` and `serve` dashboard are opt-in — background
lifecycle and a read-only HTTP view over the same store, not requirements for
MCP tool use.

lean-ctx and the built-in memory aren't substitutes for each other — one saves
tokens inside a session, the other saves the re-explaining tax across sessions.
The coordination layer is a different axis entirely: it's not about a single
session's token bill, it's about multiple agents (or multiple sessions of the
same agent, over time) staying out of each other's way and handing off work
cleanly.

**Why Rust, not Node:** Claude Code doesn't bundle or require Node.js — it's a
standalone compiled binary. A plugin whose hooks shell out to `node` breaks on
any machine that installed Claude Code without separately installing Node.
agentflare is a single static binary; the only runtime dependency is agentflare
itself.

**No plugin marketplace for Claude Code or Cursor** — `agentflare init --agent X`
writes the hook config directly into the target's own settings file (Claude
Code's `~/.claude/settings.json`, Cursor's `.cursor/hooks.json`). Codex is the
one exception: its hook system only activates through its plugin loader, so
that wiring ships as a small `.codex-plugin/` manifest instead.

## Metrics

Numbers below are each project's own published, reproducible benchmarks — attributed,
not blended into a fake combined total, and not accepted on faith. Where a claim had no
supporting evidence in its own repo, it's flagged instead of repeated. These cover the
**optimization layer** specifically — the coordination layer is too new for a
comparable benchmark suite yet (see [STATUS.md](STATUS.md)).

| Tool | Published claim | Methodology | Confidence |
|---|---|---|---|
| lean-ctx | 98.1% compression (`map` mode), 96.7% (`signatures`), ~99.99% cached re-read | CI-gated, reproducible via `lean-ctx benchmark report .`, measured on a 50-file repo with the GPT-4o tokenizer | High — real, reproducible, methodology named |
| `agentflare optimize output` | 65% avg output-token reduction (range 22–87%, 10 prompts) | Committed in `benchmarks/`/`evals/` — and its own docs flag the failure mode: ~1–1.5k input-token overhead per turn can make it net-negative on already-terse workloads (`docs/HONEST-NUMBERS.md`) | High — reproducible, unusually transparent about limits |
| `agentflare optimize code` | ~54% less code (94% ceiling on best task), ~20% cheaper, ~27% faster, 100% safe | 12 real feature tasks on a FastAPI+React repo, Haiku 4.5, n=4 — self-corrected an earlier overgeneralized single-shot figure | High — reproducible, self-corrected once already |

### Real usage, one live project

Not a demo — pulled live from the maintainer's own project while building this repo,
for a sense of scale. Not a controlled benchmark; one data point, your mileage varies.

```text
lean-ctx         34.2M tokens saved   92% compression   $88.45 saved   (lifetime; lean-ctx gain)
optimize output  1.16M tokens saved (~65%)                              (single session; via `agentflare cost`)
optimize code    code-minimalism markers logged, no token figure        (agentflare optimize code doesn't measure per-repo savings)
memory           2 sessions, 11 observations tracked, across 2 projects (agentflare memory sessions/search)
```

Check your own: `lean-ctx gain` · `agentflare cost` · `agentflare optimize code gain`/`debt` ·
`agentflare memory context`. Don't trust this table blindly either — re-run those commands yourself.

---

## Install the CLI

**Linux/macOS** (downloads a prebuilt binary, checksum-verified; builds from
source instead if run from inside a clone):
```bash
curl -fsSL https://raw.githubusercontent.com/getappz/agentflare/master/install.sh | sh
```

**Homebrew:**
```bash
brew tap getappz/agentflare
brew install agentflare
```

**Windows, build from source** (no unsigned prebuilt binary to trip an AV
heuristic):
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

The install methods above verify SHA-256 checksums by default — enough to
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

One command per tool, run once. Running it is the consent — installs happen
immediately, no separate confirm step.

```bash
agentflare init --agent claude-code    # writes ~/.claude/settings.json hooks directly, no marketplace
agentflare init --agent cursor         # writes .cursor/hooks.json directly, no marketplace
agentflare init --agent windsurf
agentflare init --agent vscode-copilot
agentflare init --agent cline
agentflare init --agent continue
```

**Codex** is the one exception — its hook system only activates through its own
plugin loader:
```
codex plugin marketplace add getappz/agentflare
codex plugin install agentflare
```
then `agentflare init --agent codex` for the rules/lean-ctx setup (Codex's
hook wiring itself comes from the plugin manifest, not `init`).

Each run: writes rule files (if absent), installs lean-ctx (native `curl | sh`
or Homebrew installer) if missing, wires hooks/MCP where the host supports
it. Detection-first — already-satisfied components are skipped, nothing gets
clobbered. Persistent memory ships in the binary itself — nothing to install
for it.

## Docs-only fallback (Aider, other AGENTS.md readers)

```bash
curl -sL https://raw.githubusercontent.com/getappz/agentflare/master/AGENTS.md > AGENTS.md
```

---

## Architecture

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
├── dashboard/                   # `agentflare serve` read-only dashboard backend
│                             # (pairs with the top-level dashboard/web static frontend)
├── vent/                         # friction capture, auto-classification into backlog items
├── mentions/                      # @mention inline reference parsing/resolution (@I/@A/@search)
├── ipc/                            # daemon transport — Unix socket / Windows named pipe
├── dev_install/                     # `agentflare dev-install` — build the current checkout,
│                                   # install over the running binary
├── update/                           # self-update check + binary swap
├── ui/                                # terminal UI helpers (spinner, cliclack prompts)
├── core/                                # small shared primitives (codesigning helpers)
├── paths.rs                # home-dir resolution (AGENTFLARE_HOME_OVERRIDE for tests —
│                           # dirs::home_dir() ignores HOME/USERPROFILE overrides on
│                           # Windows, learned the hard way)
├── state.rs                 # ~/.agentflare/state.json — on/off flag for the hooks
├── rule_text.rs              # shared rule copy (Exa, git, lean-ctx usage)
├── compact.rs                 # legacy FTS5/BM25 PreCompact scorer, superseded by
│                             # optimize/context.rs but still wired into the hook path
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

crates/                     # 17-member Cargo workspace: flare-code, flare-output, flare-search-kit,
                            # agentflare-store, agentflare-backend, agentflare-db-kit, agent-registry,
                            # skill-registry, gateway-registry, flare-docs, flare-git-core,
                            # flare-git-shim, agentflare-shim, agentflare-artifacts, agentflare-jobs,
                            # flare-proxy, flare-vault
dashboard/web/               # static frontend served by `agentflare serve`
.codex-plugin/              # Codex only — its hooks require the plugin loader
install.sh, install.ps1      # installers (checksum-verified download / local build)
.github/workflows/          # ci.yml (build+test), release.yml (cross-compile on tag)
```

Adding a new managed component means adding one entry to `components.rs` — neither
`init` nor `hook` hardcodes per-tool logic.

---

## What Gets Created

**Claude Code**: `~/.claude/rules/{exa,git,lean-ctx}.md`, `~/.claude/settings.json` hooks section, `~/.agentflare/` (includes the built-in memory database and the optimize layer's own state — no more separate `~/.config/{caveman,ponytail}/` now that both are built into the binary).

**Codex**: project-local `AGENTS.md` (only if absent), `~/.agentflare/`.

**Cursor**: project-local `.cursor/rules/agentflare.mdc`, `.cursor/hooks.json`, `~/.cursor/mcp.json`, `~/.agentflare/`.

**Windsurf/VS Code/Cline**: project-local rules file (see table above), MCP config for lean-ctx.

**Continue**: `.continue/mcpServers/agentflare.json`.

Nothing is created if it already exists.

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
