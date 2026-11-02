---
title: Coordination and orchestration
description: How agentflare's local work queue compares with Beads, Taskmaster, claude-squad, OpenHands, and hosted project tools.
---

Once more than one agent works on the same repository, the hard problem stops
being code generation and becomes ownership: who is editing which file, what is
already in review, and where a decision was recorded.

## What agentflare provides

- **Work items** (MCP `item`, CLI `agentflare item` / `agentflare work`) —
  backlog through in-review and completed, with priorities, dependencies,
  labels, comments, and plan approval gates.
- **Claims** (MCP `claim`, CLI `agentflare claim`) — path-scoped write locks
  recorded in a ledger, so two agents can be told apart even when both are
  editing the same checkout.
- **A git shim** — a `git` binary earlier on `PATH` that classifies and audits
  every invocation, and refuses edits outside a session's claimed scope.
- **Worktrees** — claiming an item through `agentflare work` provisions an
  isolated checkout, so parallel agents do not collide on the working tree.
- **Messaging** (MCP `message`) — direct messages between live sessions,
  addressed to a session, an item, an agent name, or a team.
- **Handoffs** (MCP `handoff`) — attach a work product to an item and assign it
  to another agent, with facts, findings, and decisions carried in the payload.
- **Artifacts** (MCP `artifact`) — publish HTML, markdown, or Mermaid pages that
  another agent or a human can open.
- **Workflows** (MCP `workflow`) — durable multi-step pipelines with schedules.
- **Reports** (MCP `pm`) — standup, groom, plan, health, and portfolio views
  computed from the same item store.

The store is local SQLite. There is a local dashboard (`agentflare serve`), but
not a multi-tenant hosted UI.

## How the alternatives differ

| | agentflare | Beads | Taskmaster | claude-squad | OpenHands Canvas |
|---|---|---|---|---|---|
| Core abstraction | Work items plus path claims | Dependency graph of issues | Tasks from a PRD | Terminal sessions | Agent control center |
| Storage | Local SQLite | Dolt-backed, syncable | Project files (JSON) | Git worktrees and tmux | Server-side |
| Multi-agent | Yes, with claims and messaging | Yes, via shared issue graph | Yes, one task at a time | Yes, via separate sessions | Yes, multi-backend |
| Isolation | Worktree per claimed item | Shared checkout | Shared checkout | Worktree plus tmux | Container |
| UI | Local dashboard | CLI (`bd`) | CLI and MCP | Terminal UI | Web UI |
| Runs offline | Yes | Yes, syncs when able | Yes | Yes | Self-hosted yes |

**Beads** ([steveyegge/beads](https://github.com/steveyegge/beads)) is the
closest philosophical match: an issue tracker for agents, stored in Dolt, with a
dependency graph and a `bd ready` view of unblocked work. Its strength is that
the graph itself is the coordination primitive, and clones can sync it. agentflare
adds write-scope enforcement and cross-session messaging, but its dependency
model is simpler.

**Taskmaster** ([eyaltoledano/claude-task-master](https://github.com/eyaltoledano/claude-task-master))
breaks a PRD into a task list and exposes it over MCP. It is excellent at the
front of a project — turning a spec into ordered work. It is not trying to be a
live lock manager for several agents in one checkout.

**claude-squad** ([smtg-ai/claude-squad](https://github.com/smtg-ai/claude-squad))
runs several terminal agents side by side in tmux, each in its own git worktree.
That solves isolation well. It does not give you a shared item queue, review
consensus, or a durable record of why a task was done.

**OpenHands Agent Canvas** ([All-Hands-AI/OpenHands](https://github.com/All-Hands-AI/OpenHands))
is a self-hosted control center for agents across several backends. It is the
closest thing to a web UI for orchestration, at the cost of running a service.

Two earlier options are no longer good recommendations: **vibe-kanban** has
announced it is sunsetting, and **Crystal** was deprecated and succeeded by
Nimbalyst.

Hosted tools (Linear, Jira, GitHub Projects) still win when humans across a team
need a shared board, permissions, and integrations. agentflare's queue is
local-first and machine-scoped by design; that is the trade.

## When to choose what

- **agentflare** — several agents on one machine or dev environment, and you
  want ownership enforced rather than suggested.
- **Beads** — you want a syncable dependency graph as the source of truth.
- **Taskmaster** — you are turning a spec into a task plan and mostly run one
  agent at a time.
- **claude-squad** — you want a terminal multiplexer for parallel agents and
  nothing more.
- **OpenHands Canvas** — you want a web control center and are willing to host
  it.
- **Linear / Jira / GitHub Projects** — humans outnumber agents and need a
  shared, permissioned board.

## Sources

- [Beads](https://github.com/steveyegge/beads)
- [Taskmaster](https://github.com/eyaltoledano/claude-task-master)
- [claude-squad](https://github.com/smtg-ai/claude-squad)
- [OpenHands](https://github.com/All-Hands-AI/OpenHands)
