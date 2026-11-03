---
title: Optimization
description: How agentflare lowers the token and cost overhead of a single agent session.
---

Optimization is about one agent, one session, and its token/cost overhead. Every layer
here answers some version of "why did this conversation cost more tokens than the work
actually required?"

The waste it targets:

- re-reading the same file verbatim on every tool call;
- restating a decision that was made three sessions ago;
- an agent writing three times more code than the task needed;
- a transcript that has grown too long to stay cheap to re-send.

The unit of concern is a single conversation, or a single agent's memory of past
conversations. None of this layer cares whether another agent exists.

## Layers

| Layer | What it reduces |
|---|---|
| **lean-ctx** (companion install) | Focused views of file reads, shell output, and search; compressed reads keep references for recovering detail. |
| **Memory** (built in) | Re-explaining decisions, facts, and preferences across sessions. |
| **`agentflare optimize output`** | Verbose prose in markdown files you hand it (requires model access). |
| **`agentflare optimize code`** | Over-engineered code: extra abstractions, defensive layers, and speculative generality. |
| **`agentflare optimize context`** | Transcript bloat: scores and selects relevant lines before compaction. |
| **Runtime nudges** | Session hygiene and model-routing mistakes, automatically. |
| **flare-docs** | Writing against a library version the model only remembers from training. |

[flare-docs](/docs/concepts/flare-docs/) has its own page. The command-level detail for
each layer is in the [CLI reference](/docs/reference/cli/); the MCP equivalents are in the
[MCP tools reference](/docs/reference/mcp-tools/).

## Next

- [Coordination](/docs/concepts/coordination/) — the other axis.
- [Use flare-docs](/docs/guides/flare-docs/) — a concrete optimization task.
