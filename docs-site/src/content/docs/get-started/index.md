---
title: Getting started
description: Install agentflare, connect it to your coding agent, and confirm the wiring.
---

Two commands get you running: install the binary, then run `init` once per host.

```bash
curl -fsSL https://agentflare.dev/install.sh | sh
agentflare init --agent claude-code
```

## Path

1. **[Installation](/docs/get-started/install/)** — Linux/macOS script, Homebrew, Scoop,
   `cargo install`, or build from source. Includes release verification.
2. **[Wire into your agent](/docs/get-started/wire-into-agent/)** — supported hosts, what
   `init` writes, and how to check the result with `agentflare doctor`.

Then read [Concepts](/docs/concepts/) for the mental model, or jump straight to a
[guide](/docs/guides/).
