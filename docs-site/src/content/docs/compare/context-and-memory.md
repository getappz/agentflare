---
title: Context and memory
description: How agentflare's memory and optimization layers compare with mem0, Letta, claude-mem, and LLMLingua.
---

"Context" and "memory" get used for three different jobs: fitting more into one
turn, remembering across turns, and remembering across sessions. agentflare
splits those jobs across separate tools instead of one memory service.

## What agentflare provides

- **Optimization** (`agentflare optimize`, MCP `optimize`) — three layers:
  - `output` compresses prose with an LLM, keeps a backup, and can retrieve the
    original by id.
  - `code` returns code-minimalism instructions at lite/full/ultra intensity.
  - `context` scores a session transcript for relevance when you need to trim.
- **Memory** (MCP `memory`, CLI `agentflare memory`) — observations stored in a
  local SQLite database with FTS5 retrieval, project or personal scope, topic-key
  upserts, and typed relations between observations.
- **Search** (MCP `search`) — one query can fan out across the local store,
  memory, code, and a web arm, then fuse the rankings with reciprocal rank
  fusion so incomparable score scales do not have to be normalized.
- **lean-ctx integration** — code reads and searches route through lean-ctx's
  cached, compressed reader rather than re-reading whole files.

Storage is local (SQLite on disk) and there is no hosted memory service or
account. Web search and model-backed paths (`optimize output`, the search web
arm) can still send data off the machine when you use them.

## How the alternatives differ

| | agentflare | mem0 | Letta | claude-mem | LLMLingua |
|---|---|---|---|---|---|
| Primary job | Local session memory, store search, output compaction | Memory extraction and retrieval layer | Stateful agent runtime | Session capture and recall | Prompt compression |
| Storage | SQLite + FTS5, local | Vector store and optional graph | Agent state and memory blocks | Local store plus summaries | None, transforms in flight |
| Extraction | Explicit writes and curation | Automatic fact extraction | Agent-managed memory blocks | Hook-captured sessions | None |
| Hosted option | No | Yes | Yes | Yes (CMEM Pro) | No |
| Works offline | Yes | Self-hosted yes | Self-hosted yes | Local mode yes | Yes |

[mem0](https://github.com/mem0ai/mem0) is a memory *layer*: it extracts facts
from conversations, consolidates duplicates, and retrieves them semantically.
agentflare's memory is more literal — you (or the agent) write observations, and
retrieval is keyword-first FTS5 — but it has no extraction pipeline to trust or
debug.

[Letta](https://github.com/letta-ai/letta) is a runtime for stateful agents,
with explicit memory blocks the model can edit. Its successor CLI,
[`letta-code`](https://github.com/letta-ai/letta-code), targets coding agents
directly. Choosing Letta means adopting its agent runtime; agentflare adds memory
to agents you already run.

[claude-mem](https://github.com/thedotmack/claude-mem) captures sessions through
hooks, compresses them into summaries, and injects relevant ones later. That is
close to agentflare's `optimize output` plus `memory`, but claude-mem is built
around automatic capture, while agentflare leans on explicit observation writes
and a searchable store.

[LLMLingua](https://github.com/microsoft/LLMLingua) compresses prompts at the
token level (LLMLingua-2 is the faster variant). It is complementary: it shrinks
what you send, while agentflare decides what is worth sending and what to
remember.

## Honest trade-offs

- agentflare will not silently build a knowledge base from every conversation.
  If you want automatic extraction and consolidation, mem0 or claude-mem do more
  of that for you.
- Retrieval is keyword-first. Semantic memory services handle paraphrased recall
  better out of the box.
- The payoff is inspectability: the store is a SQLite file you can query, back
  up, or delete. Keeping storage local is not the same as never leaving the
  machine when you opt into web or model-backed calls.

## When to choose what

- **agentflare** — you want local, inspectable memory bolted onto Claude Code,
  Codex, Cursor, or another agent you already use.
- **mem0** — you are building a product that needs a memory API and automatic
  fact extraction.
- **Letta** — you want the agent runtime itself to own memory and state.
- **claude-mem** — you want hands-off session capture with summaries.
- **LLMLingua** — you need prompt compression and already have a retrieval layer.

## Sources

- [mem0](https://github.com/mem0ai/mem0)
- [Letta](https://github.com/letta-ai/letta) · [letta-code](https://github.com/letta-ai/letta-code)
- [claude-mem](https://github.com/thedotmack/claude-mem)
- [LLMLingua](https://github.com/microsoft/LLMLingua)
- [lean-ctx](https://github.com/getappz/lean-ctx)
