---
title: Supporting tools
description: How agentflare's browser, search, vault, model proxy, and git shim compare with single-purpose alternatives.
---

agentflare also ships a set of smaller tools that are easy to compare
individually. Each one is deliberately narrower than the best-in-class
single-purpose product in its category.

## Browser automation

`flare_browser` (CLI `agentflare browser`, MCP `browser`) is one consolidated
verb set — open, snapshot, click, fill, read, eval, tabs, network, batch, state.
It delegates execution to the `agent-browser` sidecar, snapshots expose `@e1`
style references, and sessions are scoped per working directory so concurrent
worktrees do not collide. Secret values can be redacted from output before it
reaches the model.

| | agentflare browser | Playwright MCP | browser-use |
|---|---|---|---|
| Interface | CLI plus MCP | MCP server | Python library, hosted cloud |
| Page read | Accessibility snapshot with refs | Accessibility tree snapshot | Screenshots plus DOM |
| Isolation | Session per working directory | Browser profile | Cloud browser sessions |
| Best at | Agent-first navigation in a worktree | Playwright users who want MCP | LLM-driven browsing as a framework |

[Playwright MCP](https://github.com/microsoft/playwright-mcp) is the reference
implementation for accessibility-tree browsing from an agent. Its own docs
recommend the Playwright CLI for coding agents and reserve the MCP server for
cases that need a persistent page. [browser-use](https://github.com/browser-use/browser-use)
is a framework for LLM-driven browsing and also offers hosted browsers.

## Search

The `search` tool accepts a `type` and fans out across sources: local store,
memory, code, web, social, news, GitHub, academic, datasets, websites, weather,
financial, crypto, FX, indicators, YouTube, and Bluesky. Multi-source queries
fuse rankings with reciprocal rank fusion. Many sources work with no API key.

| | agentflare search | Exa MCP | Tavily MCP |
|---|---|---|---|
| Scope | Many source types, local plus web | Web search tuned for AI | Web search tuned for agents |
| Fusion | RRF across arms | Single provider index | Single provider index |
| Keys | Optional for many sources | Required | Required |
| Best at | One query across local state and the web | High-quality semantic web results | Agent-oriented web answers |

[Exa](https://exa.ai) and [Tavily](https://tavily.com) are purpose-built search
APIs with MCP servers. If web result quality is the whole job, they are the
stronger choice; agentflare's value is fusing web results with the local store
and memory in one call.

## Secrets

`agentflare vault` (MCP is not exposed for raw secret reads; `handoff` and
`channel_send` scan for secrets before sending) stores secrets in a local
encrypted vault. The vault uses Argon2id to derive a key-encryption key, AEAD to
protect values and the data key, and a session cache so you unlock once per
session. Global and per-project vaults merge, with project values winning, and
`agentflare run` injects them into a launched agent's environment.

| | agentflare vault | 1Password CLI | Doppler | direnv |
|---|---|---|---|---|
| Hosting | Local file | 1Password service | Hosted service | None |
| Team sharing | No | Yes | Yes | No |
| Encryption at rest | Argon2id plus AEAD | Provider-managed | Provider-managed | None |
| Injection | `agentflare run` | `op run` | `doppler run` | Shell hook |
| Best at | Per-project agent secrets offline | Teams already on 1Password | Centralized team secrets | Per-directory env vars |

[1Password CLI](https://developer.1password.com/docs/cli/), [Doppler](https://www.doppler.com),
and [direnv](https://direnv.net) solve adjacent problems. direnv is convenience,
not a secret store — `.envrc` is usually plaintext. If a team needs shared,
audited secrets, use a team secret manager.

## Model proxy and routing

`flare-proxy` is an Anthropic-shaped local proxy that forwards to Anthropic,
OpenAI, Gemini, xAI, Perplexity, and other providers, choosing a model per role
from `MODEL`, `MODEL_OPUS`, `MODEL_SONNET`, and `MODEL_HAIKU`. `agentflare wrap
--with-proxy` installs a Claude wrapper that uses it.

| | agentflare proxy | LiteLLM | claude-code-router |
|---|---|---|---|
| Shape | Local Anthropic-shaped proxy | Gateway and Python SDK | Local router for Claude Code |
| Provider breadth | Several, configured per role | Very broad | Several |
| Budgets and keys | No | Yes | No |
| Best at | Pointing one agent at a different model | Team gateways with spend control | Switching Claude Code models locally |

[LiteLLM](https://github.com/BerriAI/litellm) is the mature option when you need
provider breadth, virtual keys, and budgets. [claude-code-router](https://github.com/musistudio/claude-code-router)
is the closest match for local model switching.

## Git guardrails

agentflare installs a `git` shim earlier on `PATH`. The shim classifies and
audits every invocation, checks edits against active claims, and blocks unsafe
operations such as editing on the default branch or creating worktrees behind
the coordination layer's back.

| | agentflare git shim | pre-commit | lefthook |
|---|---|---|---|
| Layer | Every `git` invocation | Git hook events | Git hook events |
| Language | Rust | Python | Go |
| Parallel jobs | Not applicable | No | Yes |
| Best at | Enforcing agent write scope | Multi-language hook ecosystems | Fast, single-binary hooks |

[pre-commit](https://pre-commit.com) manages multi-language hooks and has a
large plugin ecosystem. [lefthook](https://github.com/evilmartians/lefthook) is
a single Go binary that runs hook jobs in parallel with glob, tag, and script
support. Those tools run at commit and push time; the shim runs at command time,
which is what lets it refuse an edit before it happens.

## Sources

- [Playwright MCP](https://github.com/microsoft/playwright-mcp) · [browser-use](https://github.com/browser-use/browser-use)
- [Exa](https://exa.ai) · [Tavily](https://tavily.com)
- [1Password CLI](https://developer.1password.com/docs/cli/) · [Doppler](https://www.doppler.com) · [direnv](https://direnv.net)
- [LiteLLM](https://github.com/BerriAI/litellm) · [claude-code-router](https://github.com/musistudio/claude-code-router)
- [pre-commit](https://pre-commit.com) · [lefthook](https://github.com/evilmartians/lefthook)
