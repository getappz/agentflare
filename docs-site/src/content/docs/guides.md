---
title: Guides
description: Task-shaped walkthroughs for flare-docs and agentflare's coordination tools, from the terminal or through your agent.
---

## flare-docs

### You're about to use a crate you've never touched before

Ask your agent to use it, and it'll check `flare-docs` on its own before writing code.
Want to see the docs yourself first?

```bash
agentflare docs get tokio
```

### You just bumped a dependency's version

Your agent's cached copy is now out of date. Tell it to refresh, or do it yourself:

```bash
agentflare docs refresh reqwest --version 0.13.0
```

### You know roughly what you need, but not the exact package

```bash
agentflare docs search "http client with retries"
```

Searches everything already cached — useful when you're not sure which crate or
package actually provides what you're after.

### A scoped npm package

```bash
agentflare docs get @types/node
```

No extra flags needed — a scoped name like `@types/node` is always npm.

### A Python package

```bash
agentflare docs get requests --ecosystem python
```

Python packages always need `--ecosystem python` — unlike npm's `@scope/pkg`, there's
no naming convention that marks a package as Python, so it can't be inferred.

### Seeing everything you've cached so far

```bash
agentflare docs list
```

Handy for checking what's already available offline before you go somewhere without a
network connection.

## Coordinating work across agents

`flare-docs` solves "my agent doesn't know the current API." The rest of agentflare's
built-in tools solve a different problem: running more than one agent — or picking work
back up days later — without two agents stepping on the same file, or a design decision
getting lost between sessions. These are all backed by a local SQLite store
(`~/.agentflare/agentflare.db`), not a hosted service.

### Claiming and working a backlog item

Your team (or your other agents) track work as items in agentflare's built-in project
tracker. Before starting on one, claim it so a second agent doesn't pick up the same
work:

```text
item(action="claim", id="42")
```

Then either work it yourself, or hand the whole claim-run-report loop to
`agentflare work`, which claims the item, runs an agent on it in an isolated worktree,
and reports the result back as a comment (with a PR link, if one was opened):

```bash
agentflare work 42 --agent claude-code --timeout 1800
```

When it's done, `item(action="release", id="42")` or `item(action="done", id="42")`
frees it up — `agentflare work` does this for you automatically on success or failure.

### Publishing a spec as a shareable artifact

Design docs and implementation plans are working documents, not files you necessarily
want committed to the repo. Publish one as an artifact instead — it gets its own URL you
can share, and later revisions update the same URL rather than minting a new one:

```text
artifact(action="publish", session_id="my-project-specs", name="2026-07-29-flare-docs-restructure",
         type="markdown", label="draft", content="...")
```

To revise it later without changing the link, pass `update_id` (the id `publish`
returned) plus `base_version` for an optimistic-concurrency check:

```text
artifact(action="publish", update_id="<id-from-first-publish>", base_version=1,
         content="...", label="reviewed")
```

`artifact(action="list", session_id="my-project-specs")` finds it again later without
you needing to remember the id.

### Getting a second opinion via review consensus

When one agent reviews its own diff, it tends to rubber-stamp its own work. Review
consensus fixes that by having one or more agents submit findings independently, then
merging and verifying them against the actual diff:

```bash
echo '[{"file":"src/lib.rs","line":42,"message":"unwrap() on a request that can fail","severity":"medium"}]' \
  | agentflare review submit --pr 406

agentflare review consensus --pr 406
```

`consensus` verifies each finding's cited line against the real diff, dedups
overlapping findings from different agents, and tags each one CONFIRMED, UNIQUE,
DISPUTED, or UNVERIFIED — so you're not left cross-referencing several agents' raw
output by hand. `agentflare review scores` shows each agent's running accuracy (verified
findings vs. total submitted) across past rounds, if you want a sense of which of your
agents' reviews to trust more.
