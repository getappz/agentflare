---
title: flare-docs
description: Always-current Rust, npm, and Python package documentation for your AI coding agent — built into agentflare, no setup required.
---

If you've ever watched your AI coding agent confidently write code against a library
version that doesn't exist anymore, `flare-docs` is the fix. It's a built-in part of
agentflare that fetches the **real, current documentation** for a Rust crate, npm
package, or Python package the moment your agent needs it — instead of relying on
whatever the model happened to memorize during training, which goes stale the day a
library ships a new version.

There's nothing to install or configure. If your agent is connected to agentflare, it
already has this available.

## What's supported

`flare-docs` covers three ecosystems today:

- **Rust** — any crate published on [docs.rs](https://docs.rs).
- **npm** — any package on the npm registry, using its published TypeScript types (or,
  for packages that don't ship their own types, the community-maintained
  [DefinitelyTyped](https://github.com/DefinitelyTyped/DefinitelyTyped) types instead).
- **Python** — any package on PyPI, using its type stubs (or, for packages that don't
  ship their own, the community-maintained [typeshed](https://github.com/python/typeshed)
  stubs instead).

Need something outside Rust/npm/Python? [Context7](https://context7.com) covers a much
broader range of languages and frameworks — see [how it compares](/docs/compare/).

## You usually don't need to specify which one

Most of the time, `flare-docs` figures out the right registry on its own:

- A scoped package name like `@types/node` or `@babel/core` is always npm — no other
  supported registry uses that naming style.
- Everything else defaults to Rust — including Python packages, which have no
  comparable naming marker, so an unscoped Python package always needs an explicit
  `ecosystem="python"`.

### If you guess wrong

Looking up a package under the wrong ecosystem doesn't just fail silently — the error
tells you which one to try instead, for example:

```
"express" was not found on docs.rs — if it is a Node package, retry with ecosystem="npm";
if it is a Python package, retry with ecosystem="python"
```

## How your agent uses it

When your agent is about to write code against a library it isn't confident about, it
can, on its own, without you prompting it to:

- **Look up a package's docs** — by name, and optionally a specific version.
- **Search** across everything it's already fetched, when it knows roughly what it
  needs but not the exact package.
- **Refresh** a package after you've upgraded it, so it isn't working from a stale
  cached copy.
- **List** everything it's already cached.

You'll typically notice this as a quiet step before the agent writes code that uses
something new to it — not something you need to ask for.

If you're setting up an MCP client yourself (rather than using agentflare's own
gateway), the tool is called `docs` and takes an `action` of `search`, `get`, `list`, or
`refresh`, plus a package name and optional ecosystem/version.

For the exact flags and parameters — from the terminal or from an MCP client — see the
[CLI reference](/docs/cli/) and [MCP tools reference](/docs/mcp-tools/). For
task-shaped walkthroughs, see [Guides](/docs/guides/).

## Why it's fast

The first time a package is looked up, `flare-docs` fetches and caches it. Every lookup
after that — by you or your agent — is instant and works offline, because it's reading
from that local cache instead of making a network call.

## Usage examples, not just API reference

Alongside the API surface, `flare-docs` pulls usage examples straight from a package's
own README or PyPI description — the maintainer's real words, filtered to drop
install/contributing/license noise. Not an LLM's summary of the docs: the actual text
the maintainer published.
