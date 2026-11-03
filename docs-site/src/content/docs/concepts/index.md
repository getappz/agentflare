---
title: Concepts
description: The mental model behind agentflare - optimization vs. coordination, and what "local-first, no daemon required" means.
---

agentflare does two things, and they solve different problems along different axes. Most
confusion about what a command or MCP tool is "for" traces back to conflating the two.
The split matters before you read the [CLI reference](/docs/reference/cli/) or
[MCP tools reference](/docs/reference/mcp-tools/).

- **[Optimization](/docs/concepts/optimization/)** — one agent, one session, and its
  token/cost overhead.
- **[Coordination](/docs/concepts/coordination/)** — several agents, or several sessions
  over time, working the same backlog without collisions.
- **[flare-docs](/docs/concepts/flare-docs/)** — the always-current package documentation
  layer that sits inside optimization.

## Why they're separate axes, not one spectrum

A team could adopt one without the other. Run a single agent solo and care only about its
token bill: optimization is the whole story. Run several agents against a shared backlog,
each individually cheap to operate: coordination is what keeps them out of each other's
way, regardless of how efficient any one of them is internally.

agentflare bundles both because most teams eventually want both, but they are different
problems with different failure modes. The tools reflect that split instead of blending
into one undifferentiated feature list.

## What "local-first, SQLite-backed, no daemon required" means

The coordination layer's state — items, claims, reviews, artifacts, handoffs, comments —
lives in local SQLite files under `~/.agentflare/`, not a hosted service. MCP tools read
and write that store directly, over the same stdio transport agentflare already exposes
for the optimization layer. There is no server to stand up and no account to create for
the core primitives: two agents on the same machine (or sharing the same filesystem) can
claim items and hand off work with nothing running in the background.

A daemon (`agentflare daemon`) exists for the inherently asynchronous pieces —
`agentflare work`'s claim → worktree → headless-agent → report loop, background webhook
delivery, scheduled workflows. If you are not using those, you do not need it running.

## Next

- [Optimization](/docs/concepts/optimization/) and [Coordination](/docs/concepts/coordination/)
- [Guides](/docs/guides/) — task-oriented walkthroughs that combine the layers.
- [How it compares](/docs/compare/) — adjacent tools, feature by feature.
- [Architecture](/docs/reference/architecture/) — how the pieces are built.
