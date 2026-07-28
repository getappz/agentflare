---
title: CLI reference
description: agentflare docs — search, get, list, and refresh from the command line.
---

The same cache backing the MCP `docs` tool is reachable directly from the CLI, under
`agentflare docs`.

## `agentflare docs search <query>`

Search cached documentation.

```bash
agentflare docs search "serde deserialize" --limit 10
```

| Flag | Default | Notes |
|---|---|---|
| `--limit` | `10` | Capped at 50 |

## `agentflare docs get <package>`

Print a package's docs, reading from cache if present — fetching and storing it
otherwise.

```bash
agentflare docs get serde --version latest
agentflare docs get hono --ecosystem npm
agentflare docs get @types/node          # scoped name -> npm, no --ecosystem needed
agentflare docs get requests --ecosystem python
```

| Flag | Default | Notes |
|---|---|---|
| `--version`, `-v` | `latest` | |
| `--ecosystem`, `-e` | inferred | `rust`, `npm`, or `python`. Defaults to `rust` unless the package name is scoped. |

## `agentflare docs list`

Lists every cached document (summaries only — see the
[MCP tool reference](/docs/mcp-tool/#list) for the paging shape; the CLI prints the same
JSON the MCP tool returns).

```bash
agentflare docs list
```

## `agentflare docs refresh <package>`

Forces a fresh fetch, bypassing the cache — use when a package shipped a new version and
the cached copy is stale.

```bash
agentflare docs refresh tokio --version 1.42.0
```

Accepts the same `--version` / `--ecosystem` flags as `get`.

## Exit codes

A store I/O failure or a fetch error prints to stderr and exits `1`. An unrecognized
`--ecosystem` value exits `2`, distinct from a runtime failure.
