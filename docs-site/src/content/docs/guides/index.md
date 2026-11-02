---
title: Guides
description: Task-shaped walkthroughs for docs lookup, multi-agent coordination, and notifications.
---

Each guide starts from a task, shows the command or MCP call, and explains what happens
underneath. Read [Concepts](/docs/concepts/) first if you want the model behind them.

| Task | Guide |
|---|---|
| Stop your agent writing against a stale library API | [Use flare-docs](/docs/guides/flare-docs/) |
| Claim work, publish a spec, or run review consensus across agents | [Coordinate agents](/docs/guides/coordinate-agents/) |
| Get a Telegram, Slack, or Discord ping when an item needs a human | [Channel notifications](/docs/guides/notifications/) |

## Common first session

1. Install and wire the CLI: [Getting started](/docs/get-started/).
2. Ask your agent to fetch a crate it does not know; it uses `flare-docs` automatically.
3. Create a work item, claim it, and let a second session pick up something else.
4. Run `agentflare serve` to see items and activity in one place.

The [CLI reference](/docs/reference/cli/) and [MCP tools reference](/docs/reference/mcp-tools/)
cover every command and tool the guides use.
