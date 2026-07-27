---
title: Using it from your agent
description: What your AI coding agent can do with flare-docs, automatically.
---

If your AI coding agent is connected to agentflare, it already has `flare-docs`
available as a tool — nothing to turn on. When it's about to write code against a
library it isn't confident about, it can:

- **Look up a package's docs** — by name, and optionally a specific version.
- **Search** across everything it's already fetched, when it knows roughly what it
  needs but not the exact package.
- **Refresh** a package after you've upgraded it, so it isn't working from a stale
  cached copy.

You don't need to prompt your agent to do this — it's just another tool it has access
to, the same way it can read a file or run a command. You'll typically notice it
happening as a quiet step before the agent writes code that uses something new to it.

## Configuring your own agent or MCP client

If you're setting up an MCP client yourself (rather than using agentflare's own
gateway), the tool is called `docs` and takes an `action` of `search`, `get`, `list`,
or `refresh`, plus a package name and optional ecosystem/version. The
[CLI reference](/docs/cli/) documents the same four operations with the exact flags —
it's the easiest way to see what's available, since the CLI and the agent-facing tool
do the same things.
