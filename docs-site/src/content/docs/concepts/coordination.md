---
title: Coordination
description: How agentflare keeps multiple agents and sessions from colliding on the same work.
---

Coordination is about running more than one agent — or more than one session of the same
agent, over time — against the same body of work. The failure modes are different from
optimization:

- two agents editing the same file at once;
- a task redone because nobody knew it was already claimed;
- a review with no record of what was actually checked;
- context that evaporates when work moves from one agent to another.

None of these make a single session cheaper to run. They make it safe and legible to have
several running at once.

## Primitives

| Primitive | What it provides |
|---|---|
| **Items and claiming** (`item`, `claim`) | A shared backlog; claim before working so two agents do not collide. |
| **Review consensus** (`review`) | Independent findings, verified against the diff, deduplicated and scored. |
| **Artifacts** (`artifact`) | Versioned, shareable specs and plans with a stable URL. |
| **Handoffs** (`handoff`, `resume`) | Context passed to a specific agent or runtime, including across agent products. |
| **Comments, labels, projects, webhooks** | Discussion, categorization, cross-project views, outbound events. |
| **Messaging** (`message`) | Realtime messages between live sessions. |
| **`agentflare work`** | Claim → isolated worktree → headless agent → report, with failover. |
| **`agentflare serve`** | Dashboard over items, agents, and activity; no login. |
| **`agentflare daemon`** | Background process for jobs, schedules, and sweeps. |

All state is local SQLite under `~/.agentflare/`. The core primitives work without the
daemon.

## Next

- [Optimization](/docs/concepts/optimization/) — the other axis.
- [Coordinate agents](/docs/guides/coordinate-agents/) — claim, publish, review, hand off.
- [Channel notifications](/docs/guides/notifications/) — alert a human when work stalls.
