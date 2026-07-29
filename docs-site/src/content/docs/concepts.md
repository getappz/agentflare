---
title: Concepts
description: The mental model behind agentflare — optimization vs. coordination, and what "local-first, no daemon required" means.
---

agentflare does two things, and they solve different problems along different axes. Most
confusion about what a given command or MCP tool is "for" traces back to conflating the
two, so it's worth being precise about the split before diving into the
[CLI reference](/docs/cli/) or [MCP tools reference](/docs/mcp-tools/).

## Optimization: shrinking a single session's bill

Optimization is about one agent, one session, and its token/cost overhead. Every layer
here — lean-ctx, the built-in memory store, `agentflare optimize output`/`code`/`context`,
the automatic runtime nudges — is answering some version of "why did this conversation
cost more tokens than the work actually required?" The waste it targets: re-reading the
same file verbatim on every tool call, restating a decision that was already made three
sessions ago, an agent writing three times more code than the task needed, a transcript
that's grown too long to stay cheap to re-send.

The unit of concern is a single conversation (or a single agent's memory of past
conversations). None of this layer cares whether another agent exists.

## Coordination: keeping multiple agents from colliding

Coordination is about running more than one agent — or more than one session of the same
agent, over time — against the same body of work. That's a different failure mode
entirely: two agents editing the same file at once, a task getting redone because nobody
knew it was already claimed, a review with no record of what was actually checked, or
context that evaporates the moment work moves from one agent to another.

Work items and claiming (`item`, `claim`), review consensus (`review`), artifacts,
handoffs, coaching nudges, the daemon, `agentflare work`, `vent`, `agentflare serve`, the
unified `search` tool, the git-aware PATH shim, and the auth vault all live on this side.
None of them make a single session cheaper to run — they make it safe and legible to have
several running at once.

## Why they're separate axes, not one spectrum

A team could adopt one without the other. Run a single agent solo, care only about its
token bill — optimization is the whole story, coordination is irrelevant. Or run several
agents against a shared backlog, each individually inexpensive to operate — coordination
is what keeps them out of each other's way, independent of how efficient any one of them
is internally. agentflare bundles both because in practice most teams eventually want
both, but they're genuinely different problems with different failure modes, and the
tools reflect that split rather than blending it into one undifferentiated feature list.

## What "local-first, SQLite-backed, no daemon required" means

The coordination layer's state — items, claims, reviews, artifacts, handoffs, comments —
lives in local SQLite files under `~/.agentflare/`, not a hosted service. MCP tools read
and write that store directly, over the same stdio transport agentflare already exposes
for the optimization layer. There's no server to stand up and no account to create for
the core primitives to work: two agents on the same machine (or sharing the same
filesystem) can claim items and hand off work without anything running in the background.

A daemon (`agentflare daemon`) does exist, but it's there for the pieces that are
inherently asynchronous — `agentflare work`'s claim → worktree → headless-agent → report
loop, background webhook delivery — not a requirement for the coordination primitives
themselves. If you're not using those, you don't need it running.

## Where to go next

- **[CLI reference](/docs/cli/)** and **[MCP tools reference](/docs/mcp-tools/)** — the
  full surface for both layers.
- **[Guides](/docs/guides/)** — task-oriented walkthroughs that combine the two.
- **[How it compares](/docs/compare/)** — how individual pieces (like `flare-docs`) stack
  up against adjacent single-purpose tools.
