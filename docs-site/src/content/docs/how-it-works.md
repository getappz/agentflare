---
title: Supported languages
description: Which languages and package registries flare-docs covers, and how it picks between them.
---

`flare-docs` currently covers three ecosystems:

- **Rust** — any crate published on [docs.rs](https://docs.rs).
- **npm** — any package on the npm registry, using its published TypeScript types (or,
  for packages that don't ship their own types, the community-maintained
  [DefinitelyTyped](https://github.com/DefinitelyTyped/DefinitelyTyped) types instead).
- **Python** — any package on PyPI, using its type stubs (or, for packages that don't
  ship their own, the community-maintained [typeshed](https://github.com/python/typeshed)
  stubs instead).

## You usually don't need to specify which one

Most of the time, `flare-docs` figures out the right registry on its own:

- A scoped package name like `@types/node` or `@babel/core` is always npm — no other
  supported registry uses that naming style.
- Everything else defaults to Rust — including Python packages, which have no
  comparable naming marker, so an unscoped Python package always needs an explicit
  `--ecosystem=python`.

If you (or your agent) need to be explicit, both the CLI and your agent can pass an
`--ecosystem`/`ecosystem` value of `rust`, `npm`, or `python` — see the
[CLI reference](/docs/cli/) and [Using it from your agent](/docs/mcp-tool/).

## If you guess wrong

Looking up a package under the wrong ecosystem doesn't just fail silently — the error
tells you which one to try instead, for example:

```
"express" was not found on docs.rs — if it is a Node package, retry with ecosystem="npm";
if it is a Python package, retry with ecosystem="python"
```

## More languages

Rust, npm, and Python are what's supported today. If you need something else,
[Context7](https://context7.com) covers a much broader range of languages and
frameworks — see [how it compares](/docs/compare/).
