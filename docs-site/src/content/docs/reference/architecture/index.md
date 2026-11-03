---
title: Architecture
description: Architecture of agentflare - a technical deep dive, checkout context, and the architectural blueprint.
---

Adapted from the repository's [ARCHITECTURE.md](https://github.com/getappz/agentflare/blob/master/ARCHITECTURE.md). Source citations below retain the audited snapshot.

For current Codex setup, use `agentflare init --agent codex` alone. Installing the
Codex plugin as well duplicates lifecycle hooks; see [Getting started](/docs/get-started/).

This document describes the local checkout inspected on **2026-10-26**, at
`a4e59d748a61dba653ff55e20052b09cb79376c9` on `docs/marketing-content`.
It documents existing behavior; it proposes no code modernization.
Source links point to the audited repository snapshot. `#L…` anchors identify lines in
the inspected snapshot; use the named symbol if later edits move a line.


## In this section

- [Technical deep dive](/docs/reference/architecture/internals/) - capabilities, detected stack, entry points, command inventory, subsystem map, runtime surface, dependencies, storage and background jobs.
- [Checkout and ecosystem](/docs/reference/architecture/checkout-and-ecosystem/) - repository identity, contributor and agent rules, developer gotchas.
- [Architectural blueprint](/docs/reference/architecture/blueprint/) - system context, containers, a representative work-item lifecycle, layering rules, cross-cutting concerns, and inferred decisions.

> **Snapshot.** The three pages below describe the checkout audited at `a4e59d7` (2026-10-26). Later commits may have moved code; link anchors are pinned to that snapshot. The CLI and MCP pages are maintained separately.

