---
title: MCP tools reference
description: The full first-party mcp__flare__* tool surface — 24 tools plus MCP Prompts.
---

Everything below is a first-party tool your agent gets once it's connected to
agentflare's MCP server (wired by `agentflare init`) — reached as `mcp__flare__<name>`.
Most are **consolidated tools**: one tool name, an `action` field selecting the
operation, so the tool list stays short instead of growing one entry per verb. See the
[CLI reference](/docs/cli/) for the equivalent command-line surface where one exists.

## Coordination

The shared backlog (items, comments, labels, assets) plus claims, handoffs, and reviews —
how agents divide and hand off work.

### `item`

Manage work items in the repo's linked project (auto-created/linked on first use).

`action`: `create`, `get`, `list`, `search`, `update`, `update_state`, `delete`, `claim`,
`heartbeat`, `release`, `done`, `cancel`, `add_label`, `remove_label`, `groom`,
`standup`, `health`.

```text
mcp__flare__item(action="list", state_group="backlog,unstarted,started")
mcp__flare__item(action="claim", id="42")
```

### `comment`

Threaded comments on an item.

`action`: `create`, `edit`, `delete`, `list`.

```text
mcp__flare__comment(action="create", item_id="<id>", body="left a note for the next pass")
```

### `label`

Labels in the repo's linked project.

`action`: `create`, `list`, `update`, `delete`.

```text
mcp__flare__label(action="list")
```

### `asset`

Attach, get, list, or delete file assets on items/projects. Attaching requires the file
to already exist under `~/.agentflare/staging/<filename>`.

```text
mcp__flare__asset(action="attach", item_id="<id>", filename="report.md")
mcp__flare__asset(action="list", item_id="<id>")
```

### `claim`

The work-claim ledger — acquire, heartbeat, release, done, or list, so parallel agents
don't duplicate work. Backs `agentflare claim` on the CLI.

`action`: `acquire`, `heartbeat`, `release`, `done`, `list`.

```text
mcp__flare__claim(action="acquire", target="issue#42", scope=["crates/foo/"])
```

### `handoff`

Assigns/creates an item for a recipient agent and attaches content to it as a versioned
asset — the recommended path for agent-to-agent work products (prefer this over
`artifact` for handoffs).

```text
mcp__flare__handoff(recipient="opencode", name="review-notes", content="please check the API design above")
```

### `webhook`

Register, list, or delete webhooks on the repo's linked workspace.

```text
mcp__flare__webhook(action="list")
```

### `project`

Info about the workspace/project this repo is currently linked to.

`action`: `info` (only one for now).

```text
mcp__flare__project(action="info")
```

### `review`

Multi-agent review consensus: submit findings, verify citations against the diff,
dedup, and tag CONFIRMED/UNIQUE/DISPUTED/UNVERIFIED; track per-agent accuracy over time.

`action`: `submit`, `consensus`, `list`, `clear`, `record`, `scores`.

```text
mcp__flare__review(action="submit", pr="123", findings=[{"file":"src/lib.rs","line":42,"message":"unwrap on user input"}])
mcp__flare__review(action="consensus", pr="123")
```

### `artifact`

Publish, list, get, diff, search, or delete live-shareable local pages. Kept for
standalone shareable pages (dashboards, reports) — for agent-to-agent handoffs, use
`handoff` instead.

`action`: `publish`, `list`, `get`, `diff`, `search`, `delete`.

```text
mcp__flare__artifact(action="publish", name="usage-report", type="markdown", content="...")
```

## Knowledge & Memory

Third-party docs, persistent memory, and the skill/search layers that help an agent
find the right context or capability.

### `docs`

On-demand third-party package/API documentation, cached. Rust via docs.rs (default),
npm via npmjs.org (`ecosystem="npm"`), Python via PyPI (`ecosystem="python"`).

`action`: `search`, `get`, `list`, `refresh`.

```text
mcp__flare__docs(action="get", package="serde", ecosystem="rust")
mcp__flare__docs(action="search", query="hono routing")
```

### `memory`

The built-in persistent memory store — compact session history, recall past
observations, curate what's kept, relate entries, and hand memory off across sessions.

`action`: `compact`, `context`, `curate`, `handoff`, `recall`, `relate`, `remember`.

```text
mcp__flare__memory(action="remember", title="db choice", content="chose sqlite over postgres for single-writer simplicity")
mcp__flare__memory(action="recall", query="why did we pick sqlite")
```

### `search`

Unified search across 17 sources via `type`: `store` (FTS docs), `memory` (brain.db
observations), `code` (leanctx), `web`, `social`, `news`, `github`, `academic`,
`datasets`, `websites`, `weather`, `financial`, `crypto`, `fx`, `indicators`, `youtube`,
`bluesky`.

```text
mcp__flare__search(type="github", query="open issues labeled good-first-issue in getappz/agentflare")
mcp__flare__search(type="web", query="astro starlight sidebar config")
```

### `skill`

Search or load a skill from the installed registry (BM25-ranked).

`action`: `search`, `load`.

```text
mcp__flare__skill(action="search", query="code review")
mcp__flare__skill(action="load", name="code-review")
```

### `skill_detect`

Classify a prompt's intent and return ranked skill matches — the mechanism behind
proactive skill suggestions.

```text
mcp__flare__skill_detect(prompt="my disk is full, need to clean up")
```

## Optimization

Model routing, code-minimalism instructions, session-health checks, and the
reversible-compression retrieval layer.

### `get_routing_suggestion`

A model-routing suggestion for a given prompt (e.g. "this looks judgment-heavy, use a
premium model").

```text
mcp__flare__get_routing_suggestion(prompt="design the auth token rotation strategy")
```

### `optimize_instructions`

Read-only: the flare-code (code-minimalism / lazy-senior-dev) instructions for a mode,
as `{mode, instructions}`. Does not change the active mode — omit `mode` for the
active/default one.

```text
mcp__flare__optimize_instructions(mode="ultra")
```

### `optimize`

Reversible-compression retrieval (CCR) — recover an original the output layer
compressed away.

`action`: `retrieve` (returns the original for a registered id), `list` (enumerates live
entries).

```text
mcp__flare__optimize(action="retrieve", id="r-a1b2c3")
```

### `check_session_health`

Whether a session should be refreshed, based on turn count and elapsed time — the
signal behind the "close the session, start fresh" guidance in long-running work.

```text
mcp__flare__check_session_health()
```

## Git / GitHub

### `flare_git`

GitHub repo operations — PRs, issues, releases, and Actions runs.

`action`: `pr_create`, `pr_list`, `pr_get`, `pr_status`, `pr_merge`, `pr_comment`,
`pr_request_review`, `pr_wait` (bounded poll loop), `issue_create`, `issue_list`,
`issue_get`, `issue_comment`, `issue_close`, `issue_label`, `release_list`,
`release_get`, `release_latest`, `release_create`, `run_list`, `run_get`, `run_rerun`,
`workflow_dispatch`.

```text
mcp__flare__flare_git(action="pr_create", title="fix: handle empty query", body="...")
mcp__flare__flare_git(action="pr_status", pr=42)
```

## Utility

### `tool`

The flare gateway entrypoint: search agentflare's own first-party tools (this whole
page) and downstream gateway-registered MCP servers by task description, or execute a
downstream one.

`action`: `search`, `execute`.

```text
mcp__flare__tool(action="search", query="ctx_shell")
mcp__flare__tool(action="execute", server="leanctx", tool="ctx_read", args={"path":"src/main.rs"})
```

### `channel_send`

Send a text message to Telegram, Slack, or Discord. The bot token must already be
stored as the gateway secret `<platform>_bot_token` (see `agentflare gateway secret
set`).

```text
mcp__flare__channel_send(platform="slack", target="C0123456", message="build is green")
```

### `vent`

Log tooling friction when the TOOLING blocks you — not the task itself: a wrong or
missing tool, a fabricated assumption, an environment gap.

```text
mcp__flare__vent(message="ctx_patch corrupted a large file", severity="high", tags=["ctx-patch"])
```

### `vent_file`

List, or file, agentflare's own tooling bugs (git shim, af-guard, item-tracker friction)
as one batched GitHub issue on getappz/agentflare.

```text
mcp__flare__vent_file(action="list")
mcp__flare__vent_file(action="file", title="...", body="...")
```

## MCP Prompts (slash commands)

A handful of surfaces are also exposed as native slash commands in hosts that support
MCP Prompts, routed through the same tools above:

- `/optimize [mode]` — switch or report the flare-code lazy-dev mode (`lite`, `full`,
  `ultra`, `off`, `status`).
- `/artifact <command>` — `publish`, `update`, `list`, `get`, `delete`.
- `/handoff <command>` — `<recipient> <brief>` to send, `inbox [me]`, or `thread <id>`.
- `/git <command>` — `install-hooks`, `install-shim`, `uninstall-shim`, `snapshot
  {list,restore,prune}`, `audit {preview,prune}`, `doctor`.
- `/optimize-review`, `/optimize-audit`, `/optimize-debt`, `/optimize-gain`,
  `/optimize-help`, `/optimize-playbook`, `/optimize-no-hallucination` — the flare-code
  sub-skills, one slash command each.
