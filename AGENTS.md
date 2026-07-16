# agentflare rules

Static fallback for agents with no MCP support and no hook mechanism (e.g. Aider).
Everything else (Claude Code, Codex, Cursor, Windsurf, VS Code/Copilot, Cline,
Continue) gets a real integration via the `agentflare` CLI — see
https://github.com/getappz/agentflare. Use this file only if your tool isn't
one of those.

## Flare optimize module

agentflare ships a single consolidated compression/optimization module (`optimize`)
with four layers:

| Layer   | Command                       | What it does                          |
|---------|-------------------------------|---------------------------------------|
| output  | `agentflare optimize output`  | LLM-based prose compression (was caveman) |
| code    | `agentflare optimize code`    | Lazy senior dev code minimalism (was ponytail) |
| context | `agentflare optimize context` | Session transcript compaction via BM25 |
| runtime | (automatic via hooks)         | Session hygiene, model routing nudges  |

Legacy commands (`agentflare flare`, `agentflare caveman`, `agentflare ponytail`)
still work as backward-compatible aliases.

`agentflare optimize retrieve <id>` (and MCP `mcp__flare__optimize
action=retrieve`) recovers an original that the output layer compressed away
(CCR pattern). lean-ctx-compressed *reads* are instead recovered via
`ctx_read mode=raw` — agentflare does not re-cache them, because lean-ctx is
a separate sidecar not in agentflare's read path.

## Context compression — lean-ctx

**MANDATORY for code intelligence — do NOT use native Grep / Read-on-full-file /
shell `cat`/`grep`/`rg`/`find` to search or read code. Route ALL of it through
lean-ctx instead.** lean-ctx is in shadow mode: native file/search/shell calls
auto-route to `ctx_*` — but the rule below is the contract so agents without
shadow routing (Aider, plain shells) still comply.

- **Code search** → `ctx_search` (action=regex | semantic | symbol), NOT Grep/grep/rg.
  - exact symbol: `ctx_search(action=symbol, name=...)`
  - by meaning: `ctx_search(action=semantic, query=...)` (uses the on-demand
    dense index — no pre-build needed)
  - by pattern: `ctx_search(action=regex, pattern=...)`
- **Callers/callees** → `ctx_callgraph` (NOT grep for "who calls X").
- **Orient in unfamiliar code** → `ctx_compose` FIRST (one call vs
  search→read→search chain).
- **Read files** → `ctx_read` (compressed reader), prefer mode=anchored/full.
  Recover a compressed read verbatim via `ctx_read mode=raw`.
- **Shell** → `ctx_shell` (auto-compresses output).

Native `cat`/`grep`/`rg`/`find`/`Read`-whole-file are ONLY for: writing files,
git status/diff you will act on, and non-code text. Everything code-intelligence
goes through lean-ctx so the index stays the single source of truth.

```bash
npm install -g lean-ctx-bin && lean-ctx onboard
```

If `ctx_*` tools are genuinely unavailable in your runtime, fall back to the
native Grep/Read — but that is the exception, and you must say so.

## Cross-session memory

agentflare ships persistent memory in the binary itself — no separate
install. Recall relevant context at session start via the CLI (works even
without MCP support):

```bash
agentflare memory context
agentflare memory search "<query>"
```

Storing new memories (`memory_remember`) is exposed as an MCP tool; if your
tool has MCP support, prefer it there. Recall-only via the CLI otherwise.

## Web search

Use Exa for internet search when available — free-tier, no API key required.

## Cargo target-dir isolation (item #133/#139)

Each claimed worktree gets its own `.cargo/config.toml` (relative
`target-dir = "target"`) so its build artifacts don't leak into a sibling
worktree. An ambient `CARGO_TARGET_DIR` env var always outranks that config
file, so `agentflare run` / `agentflare agents launch` (src/agent_launch.rs)
strip `CARGO_TARGET_DIR` from the launched agent's child env before it can
shadow the isolation, and CI's `target-dir-guard` job
(.github/workflows/ci.yml) fails the build outright if the var is set
project-wide. The residual gap is a bare shell opened inside a worktree
without going through `agentflare run` — `cargo` there will still honor an
ambient `CARGO_TARGET_DIR`.

## Git

Never add "Generated with Claude Code" or "Co-Authored-By: Claude" signatures.
Commit messages are the message only.

## Build isolation (claim worktrees)

Each claim worktree (`.worktrees/task/<id>/`) gets its own `.cargo/config.toml`
with a relative `target-dir`, so `cargo build`/`test` in one worktree can't
reuse another worktree's stale local-crate artifacts (item #133; cargo
#12516/#14053/#7740). If `sccache` is on `PATH`, the same config also sets it
as `rustc-wrapper` with `SCCACHE_BASEDIRS` pointed at the worktree's own path,
so registry-dependency compiles still share a cache across worktrees.

**Known gap:** an ambient `CARGO_TARGET_DIR` environment variable always
overrides that config file (Cargo's CLI-flag > env-var > config-file
precedence) — no per-worktree config can outrank it. If you have
`CARGO_TARGET_DIR` set globally, either unset it or trust CI over local test
runs in a claim worktree (tracked in item #139).
