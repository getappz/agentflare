---
title: Examples
description: Everyday scenarios for using flare-docs, from the terminal or through your agent.
---

## You're about to use a crate you've never touched before

Ask your agent to use it, and it'll check `flare-docs` on its own before writing code.
Want to see the docs yourself first?

```bash
agentflare docs get tokio
```

## You just bumped a dependency's version

Your agent's cached copy is now out of date. Tell it to refresh, or do it yourself:

```bash
agentflare docs refresh reqwest --version 0.13.0
```

## You know roughly what you need, but not the exact package

```bash
agentflare docs search "http client with retries"
```

Searches everything already cached — useful when you're not sure which crate or
package actually provides what you're after.

## A scoped npm package

```bash
agentflare docs get @types/node
```

No extra flags needed — a scoped name like `@types/node` is always npm.

## A Python package

```bash
agentflare docs get requests --ecosystem python
```

Python packages always need `--ecosystem python` — unlike npm's `@scope/pkg`, there's
no naming convention that marks a package as Python, so it can't be inferred.

## Seeing everything you've cached so far

```bash
agentflare docs list
```

Handy for checking what's already available offline before you go somewhere without a
network connection.
