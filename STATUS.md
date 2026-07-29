# Project status

agentflare is **Beta**: actively developed, in daily real-world use, but not
yet making stability guarantees across its whole surface. This doc splits
that surface into what's settled vs. what's still moving, so you can decide
what's safe to build automation against today.

This is a snapshot, not a contract — it will drift out of date faster than
the README. If something here looks wrong, check the code and open an issue.

## Stable — safe to depend on

- **lean-ctx integration** (`agentflare init`, hook wiring for context
  compression). CI-gated, benchmarked, in daily use.
- **Built-in memory** (`agentflare memory ...` CLI, the underlying SQLite/FTS5
  store). Storage format and CLI surface are settled.
- **`agentflare optimize output`/`code`** (formerly the separate Caveman/Ponytail
  plugins, now built into the binary) for Claude Code. Stable.
- **`agentflare init --agent <X>` / `agentflare update` / `agentflare
  uninstall`** — the cross-tool setup commands themselves (not everything
  they wire up, see below).
- **Cost tracking** (`analytics.db` rollup, `agentflare cost`) — pure cache
  over JSONL session transcripts, rebuildable, format is internal-only
  anyway.

## Actively changing — expect breakage, pin a version if you depend on these

- **Work-item backend** (`item`, `claim` MCP tools) — field names, state
  machine, and search behavior are still being iterated on.
- **Review/consensus** (`review` MCP tool) — finding schema and scoring
  methodology are not finalized.
- **Coaching rules** — the on-disk `coaching-<id>.md` trigger format has
  already changed shape more than once in the same week this doc was
  written (keyword lists → BM25 auto-match). Don't script against the
  `# Trigger:` line format without expecting to update the script.
- **Artifacts / handoff** (`artifact`, `handoff` MCP tools) — the envelope
  shape (addressing, threading, versioning) is still settling.
- **MCP tool surface generally** — tool names, parameter shapes, and which
  tools exist at all are expected to change without a major version bump
  until this section says otherwise.

## Known gaps, not yet addressed

- `agentflare update` and `install.sh` compare versions by **string
  inequality**, not semver ordering (`src/update.rs::check_for_update`).
  There's no protection today against a misconfigured "latest" GitHub
  release causing an accidental downgrade. Fix tracked separately from any
  version-scheme change.
- `agentflare daemon` (background lifecycle: start/stop/status, autostart) now
  exists — it backs `agentflare serve`'s dashboard and is opt-in (`daemon
  enable`), not started by `init`. Real-time coaching triggers off the
  work-item backend are still out of scope until a dedicated design lands.
- Auto-generated coaching-rule suggestions (from usage patterns/insights) are
  proposed but not implemented; adopting that depends on the contextual
  coaching-trigger work landing first, so growing rule counts don't blow the
  per-session token budget the way blanket `SessionStart` injection used to.

## Versioning

`Cargo.toml`'s version number does **not** currently track this Beta status
(see "Known gaps" above for why a version-scheme change is riskier than it
looks) — a 1.x version number here is not a stability claim about the whole
CLI. Trust this doc and the code over the version number for now.
