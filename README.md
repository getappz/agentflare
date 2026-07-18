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

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Status: Beta](https://img.shields.io/badge/status-beta-yellow.svg)](STATUS.md)

</div>

---

## Status: Beta

agentflare is under active development. The optimization layer (lean-ctx
integration, memory, Caveman/Ponytail wiring) is the most mature part — CI-gated,
tested, in daily use. The multi-agent coordination layer (tasks, review, coaching,
artifacts, handoffs) is newer and still finding its shape: CLI flags, MCP tool
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
| **Caveman** (Claude Code only) | conversation verbosity, ~65% | companion plugin |
| **Ponytail** (Claude Code only) | code-writing over-engineering | companion plugin |

**2. Coordination** — a lightweight, local-first backend for running *more than
one* agent (or agent session) against the same body of work, exposed as MCP
tools any MCP-capable agent can call:

| Capability | What it's for |
|---|---|
| **Work items** (`item`, `claim`) | A shared backlog — create/list/search items, claim one before working it so two agents don't collide, mark done. |
| **Review** (`review`) | Submit findings against a diff/PR, run consensus across reviewers, track scores over time. |
| **Artifacts** (`artifact`) | Publish specs, plans, and docs as versioned, shareable pages — a durable handoff surface between agents (and to you) instead of scratch files that vanish with the session. |
| **Handoff** (`handoff`) | Pass context to a specific agent/runtime, addressed and threaded, when work moves from one agent to another. |
| **Coaching** (`coaching` CLI + hook) | Small persistent nudges surfaced to an agent — at session start today, moving toward contextual triggers (tool-name and prompt-relevance) so rules only show up when actually relevant. |
| **Comments, labels, projects, webhooks, channel_send** | Threaded discussion on items, categorization, cross-project views, and outbound notifications. |

Everything above is local-first (SQLite-backed), no daemon, reachable over the
same stdio MCP transport agentflare already exposes for the optimization layer.

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
| Caveman | 65% avg output-token reduction (range 22–87%, 10 prompts) | Committed in `benchmarks/`/`evals/` — and its own docs flag the failure mode: ~1–1.5k input-token overhead per turn can make it net-negative on already-terse workloads (`docs/HONEST-NUMBERS.md`) | High — reproducible, unusually transparent about limits |
| Ponytail | ~54% less code (94% ceiling on best task), ~20% cheaper, ~27% faster, 100% safe | 12 real feature tasks on a FastAPI+React repo, Haiku 4.5, n=4 — self-corrected an earlier overgeneralized single-shot figure | High — reproducible, self-corrected once already |

### Real usage, one live project

Not a demo — pulled live from the maintainer's own project while building this repo,
for a sense of scale. Not a controlled benchmark; one data point, your mileage varies.

```
lean-ctx   34.2M tokens saved   92% compression   $88.45 saved   (lifetime; lean-ctx gain)
caveman    1.16M tokens saved (~65%)                              (single session; caveman-stats hook)
ponytail   23 `ponytail:` shortcut markers logged, no token figure (ponytail doesn't measure per-repo savings)
memory     2 sessions, 11 observations tracked, across 2 projects  (agentflare memory sessions/search)
```

Check your own: `lean-ctx gain` · `/caveman-stats` (Claude Code) · `ponytail-debt` skill ·
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

**Windows, Scoop** (prebuilt binary — unsigned, like most small Rust CLIs;
Defender/SmartScreen false-positives are possible, report an issue if hit):
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
├── paths.rs             # home-dir resolution (AGENTFLARE_HOME_OVERRIDE for tests —
│                         # dirs::home_dir() ignores HOME/USERPROFILE overrides on
│                         # Windows, learned the hard way)
├── state.rs              # ~/.agentflare/state.json — on/off flag for the hooks
├── rule_text.rs           # shared rule copy (Exa, git, lean-ctx usage)
├── memory/                # built-in persistent memory (SQLite + FTS5)
├── compact.rs             # ephemeral FTS5/BM25 relevance scorer (PreCompact hook)
├── coaching/               # session nudges: rule storage, CRUD, CLI presentation
├── claims.rs, review.rs    # work-item claiming, review/consensus (coordination layer)
├── artifacts.rs, channels.rs   # artifact publishing, outbound notifications
├── mcp_server.rs, mcp_prompts.rs  # MCP stdio server exposing both layers as tools
├── auth.rs, auth_crypt.rs, auth_db.rs, auth_runner.rs  # auth profile vault
├── components.rs          # registry: each entry checks + fixes itself, host-aware
├── init.rs                # `agentflare init --agent X` — runs every component,
│                           # wires hooks directly for claude-code/cursor
├── hook.rs                # `agentflare hook session-start|prompt-submit|... --agent X`
└── main.rs                 # clap CLI, dispatch across 45 modules

.codex-plugin/              # Codex only — its hooks require the plugin loader
install.sh, install.ps1      # installers (checksum-verified download / local build)
.github/workflows/          # ci.yml (build+test), release.yml (cross-compile on tag)
```

Adding a new managed component means adding one entry to `components.rs` — neither
`init` nor `hook` hardcodes per-tool logic.

---

## What Gets Created

**Claude Code**: `~/.claude/rules/{exa,git,lean-ctx}.md`, `~/.claude/settings.json` hooks section, `~/.config/{caveman,ponytail}/config.json`, `~/.agentflare/` (includes the built-in memory database).

**Codex**: project-local `AGENTS.md` (only if absent), `~/.agentflare/`.

**Cursor**: project-local `.cursor/rules/agentflare.mdc`, `.cursor/hooks.json`, `~/.cursor/mcp.json`, `~/.agentflare/`.

**Windsurf/VS Code/Cline**: project-local rules file (see table above), MCP config for lean-ctx.

**Continue**: `.continue/mcpServers/agentflare.json`.

Nothing is created if it already exists.

---

## Uninstall

Remove the binary (see Install section above), then remove whatever `init`
wrote for the hosts you set up — see "What Gets Created" above. Ponytail/
Caveman plugins themselves stay installed (uninstall separately if wanted).

---

<div align="center">

MIT License

</div>
