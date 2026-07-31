---
title: CLI reference
description: The full agentflare command surface — setup, agents, optimization, coordination, and ops.
---

`agentflare` (no subcommand) prints the branding banner and version. Every subcommand
below is invoked as `agentflare <name> ...`. Flags shown are the ones you'll actually
reach for — pass `--help` on any subcommand for the complete, always-current list.

## Setup

### `agentflare init --agent <host>`

Sets up an agent host: writes rule files (if not already present), installs lean-ctx if
missing, wires hooks/MCP directly into the host's own config, and registers the flare
gateway. Detection-first and idempotent — safe to re-run after an upgrade.

```bash
agentflare init --agent claude-code
agentflare init --agent cursor -y
```

| Flag | Notes |
|---|---|
| `--agent` | Required. One of `claude-code`, `cursor`, `windsurf`, `vscode-copilot`, `cline`, `continue`, `codex`, `opencode`. |
| `-y`, `--yes` | Skip confirmation prompts. |

Codex needs its plugin installed first (`codex plugin marketplace add getappz/agentflare
&& codex plugin install agentflare`) before `agentflare init --agent codex` — the hook
wiring itself comes from the plugin manifest, not `init`. See
[Getting started](/docs/getting-started/) for the full walkthrough.

### `agentflare hook <event>`

Invoked by the host's own hook system — you won't normally type this yourself, `init`
wires it into the host's config. Auto-detects the calling agent via a parent-process
walk when `--agent` is omitted.

```bash
agentflare hook session-start
agentflare hook pre-tool-use --agent claude-code
```

Events: `session-start`, `prompt-submit`, `pre-tool-use`, `post-tool-failure`,
`session-end` (no-op, kept for upgrade compatibility), `pre-compact` (deprecated no-op).

### `agentflare doctor`

Health-check sweep over worktrees, claims, and config wiring.

```bash
agentflare doctor
agentflare doctor --agent claude-code --json
```

| Flag | Notes |
|---|---|
| `--agent` | Check only this agent's wiring instead of every agent detected on PATH. |
| `--json` | Machine-readable output. |

### `agentflare alias [preferred]`

Shell alias management — point a short name at your preferred agent invocation.

```bash
agentflare alias claude-code --print
agentflare alias claude-code --shell zsh --yes
```

| Flag | Notes |
|---|---|
| `--force` | Overwrite an existing alias. |
| `--print` | Print what would be written without touching shell config. |
| `--yes` | Skip confirmation. |
| `--shell` | Target shell (default: detected). |
| `--profile` | Target shell profile file. |
| `--json` | Machine-readable output. |

### `agentflare update [version]`

Self-update check, with an MCP-safe binary replacement (same swap mechanism
`dev-install` uses).

```bash
agentflare update --check
agentflare update 0.9.2
```

| Flag | Notes |
|---|---|
| `--check` | Report whether an update is available without installing it. |
| `--quiet` | Suppress non-error output. |

### `agentflare uninstall`

Removes agentflare and everything `init` wrote — including cleaning up legacy
`~/.config/{ponytail,caveman}` directories from earlier tool names.

```bash
agentflare uninstall --dry-run
agentflare uninstall --keep-config
```

| Flag | Notes |
|---|---|
| `--dry-run` | Report what would be removed without removing it. |
| `--keep-config` | Leave config files in place. |
| `--keep-binary` | Leave the installed binary in place. |

### `agentflare dev-install`

Builds the current source tree and installs it over the running binary — for working
inside an agentflare checkout. Builds `--release` by default, verifies the binary, then
swaps it into place.

```bash
agentflare dev-install
agentflare dev-install --debug --dry-run
```

| Flag | Notes |
|---|---|
| `--debug` | Build in debug mode instead of release. |
| `--dry-run` | Build and verify, but report what would be installed without replacing. |

## Running agents

### `agentflare run <agent>`

Launches an agent through mise (so mise-managed tools are on PATH) with `.dev.vars` env
injected.

```bash
agentflare run claude-code --env staging
agentflare run claude-code --print "summarize this repo" --timeout 60
```

| Flag | Notes |
|---|---|
| `--env` | Load `.dev.vars.<stage>` instead of `.dev.vars`. |
| `--model` | Model override (interactive mode only). |
| `--mode` | Mode override (interactive mode only). |
| `--print <prompt>` | Run non-interactively on this prompt, print the reply, exit. Cannot be combined with `--model`/`--mode`/`--env`/trailing args. |
| `--timeout` | Seconds before a `--print` run is killed (default `120`). |

### `agentflare agents <action>`

Headless agent invocation and lifecycle management across every registered agent CLI.

```bash
agentflare agents list --json
agentflare agents doctor
agentflare agents install codex
agentflare agents launch claude-code --model opus -- --resume
```

Actions: `list`, `doctor`, `install <agent>`, `update <agent>`, `uninstall <agent>`,
`launch <agent> [--model] [--mode] [-- args...]`. `install`/`update`/`uninstall` accept
`--dry-run`.

### `agentflare work <target> --agent <agent>`

Autonomous claim → worktree → headless agent → report-back. Claims a work item, checks
out an isolated worktree, runs the agent on a prompt built from the item's
name/description plus prior discussion, then comments the result (or PR) back onto the
item — releasing the claim on any failure.

```bash
agentflare work 42 --agent claude-code --timeout 1800
agentflare work issue#7 --agent claude-code --max-turns 40 --max-cost-usd 2.5 --notify opencode
```

| Flag | Notes |
|---|---|
| `--agent` | Required. Must support headless/print mode. |
| `--timeout` | Headless run timeout in seconds (default `1800`). |
| `--max-turns` | Max agent turns before forced stop (Claude Code only). |
| `--max-cost-usd` | Max cost in USD before forced stop (Claude Code only). |
| `--notify` | Recipient for a handoff artifact describing the outcome. |

## Optimization

### `agentflare cost`

Token/cost usage reporting.

```bash
agentflare cost --days 7
agentflare cost --by-project
```

| Flag | Notes |
|---|---|
| `--days` | Restrict to the last N days. |
| `--by-project` | Break totals down per project. |

### `agentflare optimize` (aliases: `flare`, `opt`)

Three ports under one command, plus status/retrieve:

**`output`** — prose compression (formerly Caveman):

```bash
agentflare optimize output compress notes.md --backup sibling
```

**`code`** — code minimalism / lazy-senior-dev mode (formerly Ponytail):

```bash
agentflare optimize code status
agentflare optimize code set ultra
agentflare optimize code review    # print the over-engineering-review skill body
agentflare optimize code audit     # whole-repo audit skill
agentflare optimize code debt      # harvest flare-code: shortcut comments
agentflare optimize code gain      # measured-impact scoreboard
```

`code` subcommands: `status`, `set <mode>`, `default <mode>`, `off`, `review`, `audit`,
`debt`, `gain`, `info`, `playbook`, `no-hallucination`, `hook <event>` (internal, called
by the host's own hooks).

**`context`** — BM25 session-transcript relevance scoring:

```bash
agentflare optimize context score transcript.jsonl "what changed in auth.rs"
```

**Status and retrieve**:

```bash
agentflare optimize status
agentflare optimize retrieve --list
agentflare optimize retrieve r-a1b2c3
```

`retrieve <id>` recovers an original that the output layer compressed away (CCR — see
`--list` for registered ids).

## Coordination

### `agentflare claim <action>`

Claim GitHub issues/PRs (or scoped paths) so parallel agents don't duplicate work.
Backed by a leased ledger in `~/.agentflare/agentflare.db`.

```bash
agentflare claim acquire issue#42 --scope crates/foo/ --scope docs/foo/
agentflare claim heartbeat issue#42
agentflare claim done issue#42
agentflare claim release issue#42
agentflare claim list --all-repos
```

Actions: `acquire <target> [--repo] [--scope ...]` (steals only stale/done claims),
`heartbeat <target>`, `release <target>`, `done <target>`, `list [--repo] [--all]
[--all-repos]`.

### `agentflare review <action>`

Multi-agent review consensus: finders submit findings, agentflare verifies citations
against the diff, dedups, and tags CONFIRMED/UNIQUE/DISPUTED/UNVERIFIED.

```bash
echo '[{"file":"src/lib.rs","line":42,"message":"unwrap on user input"}]' | agentflare review submit --pr 123
agentflare review consensus --pr 123 --base master --head HEAD
agentflare review list --pr 123
agentflare review scores --all-repos --json
```

Actions: `submit [--pr] [--agent] [--file] [--repo]` (reads findings JSON from `--file`
or stdin), `consensus [--pr] [--base] [--head] [--json]`, `list`, `clear`, `record` (save
this round's per-agent accuracy), `scores [--all-repos] [--json]`.

### `agentflare handoff <recipient> [file]`

Hands a work product to another agent's inbox — publishes an artifact with a handoff
envelope; the recipient reads it via `/handoff inbox` or the `handoff`/`artifact_get` MCP
tools.

```bash
agentflare handoff opencode review-notes.md
agentflare handoff codex --content "please review the API design above" --thread t-pr42
```

| Flag | Notes |
|---|---|
| `--content` | Inline content instead of a file (mutually exclusive with `file`). |
| `--thread` | Thread id grouping an exchange (default: freshly generated). |
| `--reply-to` | Artifact id this replies to. |
| `--name` | Artifact name (default: file stem, or `"handoff"`). |
| `--session` | Session id for grouping (default: `handoffs`). |
| `--sender` | Sender identity (default: detected host agent, else `"cli"`). |

### `agentflare channel send`

Send a message to Telegram/Slack/Discord using a bot token already stored in the
gateway's encrypted secret store.

```bash
agentflare channel send --to slack --target C0123456 "build is green"
```

### `agentflare coaching <action>`

CRUD for persistent nudge rules — advisory or MANDATORY (hard-denied by the PreToolUse
hook when violated), triggered by tool name or BM25-scored against every prompt.

```bash
agentflare coaching list
agentflare coaching apply my-rule --title "Use X" --body "Always do Y" --trigger-auto
agentflare coaching enforce my-rule
agentflare coaching sync --agent claude-code
```

Actions: `list`, `apply <id> --title --body [--trigger-tool ...] [--trigger-auto]
[--tier] [--sync ...]`, `remove <id>`, `enforce <id> [--off]`, `sync [--agent]`.

## Git & worktrees

### `agentflare git <command>`

Branch-protection/provenance hooks, the `git` PATH shim, and worktree/recovery
tooling — the shell-agnostic enforcement boundary a PreToolUse hook alone can't provide
(a `git commit` run through a shell tool slips past that).

```bash
agentflare git install-hooks --yes
agentflare git install-shim --binary ./target/release/flare-git-shim
agentflare git uninstall-shim
agentflare git snapshot list
agentflare git snapshot restore --yes
agentflare git snapshot prune --keep 5
agentflare git audit preview
agentflare git audit prune --all
agentflare git doctor --format json --reclaim
```

- `install-hooks [--yes]` — installs pre-commit/pre-push/prepare-commit-msg/
  reference-transaction hooks into `.githooks/` and sets `core.hooksPath`.
- `install-shim --binary <path>` / `uninstall-shim` — install/remove the
  flare-git-shim binary as `git` on PATH so every git invocation gets classified.
  Escape hatches: `AGENTFLARE_GIT_BYPASS=1`, `AGENTFLARE_GIT_BYPASS_AGENT=<name>`,
  `AGENTFLARE_GIT_BYPASS_UNTIL=<unix-epoch>`.
- `snapshot list|restore [id] [--yes]|prune [--keep N]` — recovery snapshots taken
  automatically before a destructive git op.
- `audit preview|prune <names...|--all>` — list/remove orphaned worktree directories
  (snapshots taken first).
- `doctor [--format text|json|markdown] [--reclaim] [--force] [--staleness-days N]` —
  health sweep over all claim worktrees; exits `1` if violations are found.

## Auth & secrets

### `agentflare auth <action>`

Auth profile vault: per-agent credential rotation, cooldown tracking, health scoring,
and encrypted profile storage.

```bash
agentflare auth catalog --json
agentflare auth backup claude-code work
agentflare auth activate claude-code work --reload-daemon
agentflare auth status --agent claude-code
agentflare auth rotate claude-code --algorithm smart
agentflare auth cooldown list
agentflare auth isolate add claude-code work --shallow
```

Actions: `backup`, `activate [--reload-daemon]`, `status [agent]`, `catalog`, `ls
<agent>`, `clear <agent>`, `delete`, `rename`, `rotate [--algorithm]`, `next
[--algorithm]`, `pick`, `cooldown {set,list,clear}`, `alias`, `project {set,unset}`, `run
<agent> [-- args]`, `isolate {add,ls,delete}`, `exec`, `login`. All accept `--json`.

### `agentflare vault <action>`

Local encrypted secret vault (separate from `auth`'s per-agent profile store) — used to
unlock a session passphrase, cache the derived key, and manage stored secrets (e.g. bot
tokens for `channel send`). Values for `set` are always read from stdin, never a CLI
argument, so they never land in shell history; `list` only ever prints names.

```bash
agentflare vault unlock
agentflare vault lock
agentflare vault env
echo "$SLACK_BOT_TOKEN" | agentflare vault set slack_bot_token
agentflare vault list
agentflare vault remove slack_bot_token
```

Actions: `unlock [--stdin]`, `lock`, `env` (prints vault env vars for the current
project), `set <name>`, `list`, `remove <name>`.

### `agentflare gateway secret <action>` (deprecated)

Alias for `agentflare vault set|list|remove`, kept for backward compatibility. Prefer
`agentflare vault` directly — secret management lives there now so there's one place to
manage vault secrets instead of two.

```bash
echo "$SLACK_BOT_TOKEN" | agentflare gateway secret set slack_bot_token
agentflare gateway secret list
agentflare gateway secret remove slack_bot_token
```

## Knowledge

The same cache backing the MCP `docs` tool is reachable directly from the CLI, under
`agentflare docs`.

### `agentflare docs search <query>`

Search cached documentation.

```bash
agentflare docs search "serde deserialize" --limit 10
```

| Flag | Default | Notes |
|---|---|---|
| `--limit` | `10` | Capped at 50 |

### `agentflare docs get <package>`

Print a package's docs, reading from cache if present — fetching and storing it
otherwise.

```bash
agentflare docs get serde --version latest
agentflare docs get hono --ecosystem npm
agentflare docs get @types/node          # scoped name -> npm, no --ecosystem needed
agentflare docs get requests --ecosystem python
```

| Flag | Default | Notes |
|---|---|---|
| `--version`, `-v` | `latest` | |
| `--ecosystem`, `-e` | inferred | `rust`, `npm`, or `python`. Defaults to `rust` unless the package name is scoped. |

### `agentflare docs list`

Lists every cached document (summaries only — see the
[MCP tools reference](/docs/mcp-tools/#docs) for the paging shape; the CLI prints the
same JSON the MCP tool returns).

```bash
agentflare docs list
```

### `agentflare docs refresh <package>`

Forces a fresh fetch, bypassing the cache — use when a package shipped a new version and
the cached copy is stale.

```bash
agentflare docs refresh tokio --version 1.42.0
```

Accepts the same `--version` / `--ecosystem` flags as `get`.

**Exit codes**: a store I/O failure or a fetch error prints to stderr and exits `1`. An
unrecognized `--ecosystem` value exits `2`, distinct from a runtime failure.

### `agentflare memory <command>`

The built-in persistent memory store — sessions, observations, FTS5 search.

```bash
agentflare memory context --project myrepo
agentflare memory search "why did we pick sqlite" --limit 5
agentflare memory sessions --limit 10
agentflare memory observations --project myrepo
agentflare memory backfill-embeddings --batch 200
```

Commands: `context [--project] [--session-id]`, `search [query] [--project] [--limit]`,
`sessions [--project] [--limit]`, `observations [--project] [--limit]`,
`backfill-embeddings [--batch]` (requires a build with `--features semantic` and a
downloaded embedding model).

### `agentflare skill <action>`

The skill registry CLI — BM25 search, install/list/remove, remote registries and hubs,
eval, export/import, and stack-based provisioning.

```bash
agentflare skill search "code review"
agentflare skill install code-review
agentflare skill list
agentflare skill registry add gh:myorg/skills
agentflare skill eval
agentflare skill export skills-bundle.json
agentflare skill import skills-bundle.json
agentflare skill hub pull https://hub.example.com/bundle
agentflare skill provision . --yes
agentflare skill snooze code-review --days 14
agentflare skill dismiss code-review
```

Actions: `search <query>`, `install <name>`, `list`, `remove <name>`, `registry
{add,remove,list}`, `eval` (Hit@1/Hit@3/MRR/nDCG against a fixed query set; non-zero
exit if any metric is below its floor), `export [output]`, `import <path>`, `hub
{pull,push} <url>`, `snooze <name> [--days]`, `dismiss <name>`, `provision <path>
[--yes]` (detects the target repo's stack from its manifest files and recommends
indexed skills; dry run unless `--yes`).

## Ops

### `agentflare daemon <command>`

Background process lifecycle: PID file, flock lock, Unix socket / Windows named pipe
IPC, launchd/systemd autostart, cached update checks.

```bash
agentflare daemon start
agentflare daemon status
agentflare daemon enable   # install autostart
agentflare daemon stop
```

Commands: `start`, `stop`, `restart`, `status`, `enable`, `disable`.

### `agentflare serve`

Read-only dashboard.

```bash
agentflare serve --port 35273 --open
```

| Flag | Default | Notes |
|---|---|---|
| `--port` | `35273` | `0` = auto-assign. |
| `--host` | `127.0.0.1` | `0.0.0.0` shares with your LAN. |
| `--open` | | Open the dashboard in your browser once it's up. |
| `--yes-expose` | | Required whenever `--host` isn't `127.0.0.1`/`localhost`/`::1` — acknowledges exposing PM/cost/webhook data with no authentication. |

### `agentflare artifacts`

Serves live-shareable artifact pages (the same store `handoff` and the MCP `artifact`
tool publish to).

```bash
agentflare artifacts --port 8080 --host 0.0.0.0
```

| Flag | Default | Notes |
|---|---|---|
| `--port` | `0` (auto) | TCP port. |
| `--host` | `127.0.0.1` | Interface to bind. |
| `--dir` | `~/.agentflare/artifacts` | Storage directory. |

### `agentflare vent <action>`

Logs tooling friction (wrong/missing tool, fabricated assumption, environment gap) to an
append-only per-repo JSONL, auto-classified and auto-filed.

```bash
agentflare vent say "ctx_patch corrupted a large file" --severity high --tag ctx-patch
agentflare vent consolidate
agentflare vent list --actionable
agentflare vent file --title "..." --body "..."
```

Actions: `say <message> [--severity] [--tag ...]`, `consolidate` (triage buffered vents;
also runs automatically once per turn), `file [--title] [--body]` (list, or with both
flags, file pending agentflare-core vents as one batched GitHub issue on
getappz/agentflare), `list [--actionable]`.

## Utility

### `agentflare mcp`

Starts the MCP stdio server. This is what `init` wires your host's MCP config to run —
you won't normally invoke it directly.

```bash
agentflare mcp
```

### `agentflare about` (alias: `logo`)

Prints the branding banner, version, and where to go next. Also the target of a bare
`agentflare` invocation with no subcommand.

```bash
agentflare about
```

## Slash commands

Several CLI surfaces are also exposed as MCP Prompts (native slash commands in hosts
that support them): `/optimize`, `/artifact`, `/handoff`, `/git`, and the
`/optimize-{review,audit,debt,gain,help,playbook,no-hallucination}` sub-skills. See the
[MCP tools reference](/docs/mcp-tools/#mcp-prompts-slash-commands) for the full list.
