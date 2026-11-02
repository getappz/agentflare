---
title: Documentation lookup
description: How flare-docs compares with Context7, Ref, DeepWiki, web search, and the model's own memory.
---

Every coding agent eventually writes code against an API it half-remembers. The
fixes differ mostly in *where the answer comes from* and *who pays for the
lookup*.

## What flare-docs does

`flare-docs` (CLI `agentflare docs`, MCP tool `docs`) fetches documentation for
a single package, on demand, from that package's primary registry:

- **Rust** — the crate's docs.rs rustdoc JSON.
- **npm** — the package's own published TypeScript declarations, falling back to
  `@types/*` when the package ships untyped.
- **Python** — the package's PEP 561 `.pyi` stubs, falling back to typeshed.

Results are indexed into the local agentflare store, so a second lookup for the
same version is served from disk. Usage examples are pulled verbatim from the
package's own README or PyPI description, with install/contributing/license
noise filtered out — they are the maintainer's words, not a summary of them.

## How the alternatives differ

| | flare-docs | Context7 | Ref | DeepWiki |
|---|---|---|---|---|
| Shape | Docs fetched per package into a local cache | Hosted docs index exposed as an MCP server | Hosted search + read tools over a docs index | Generated wiki pages for a GitHub repository |
| Primary source | docs.rs, npm declarations, PyPI stubs | Its own crawling and indexing pipeline | Its own docs index, read on demand | The repository's code and prose |
| Coverage | Rust, npm/TypeScript, Python | Broad, most popular languages and frameworks | Broad, web and library documentation | Public GitHub repositories |
| Runs offline | Yes, once cached | No | No | No |
| Account needed | No | API key raises the free rate limit | Free tier, key for higher limits | No, for public repos |
| Unit of lookup | A package and version | A library, by name | A query or URL | A whole repository |

[Context7](https://context7.com) is the closest direct alternative: a hosted MCP
server with much broader language coverage and a large pre-built index. If you
work outside Rust, npm, and Python, it is the better choice today.

[Ref](https://github.com/ref-tools/ref-tools-mcp) takes a search-then-read
approach: an agent searches the docs index, then reads the one page it needs,
which keeps tokens low on large documentation sets.

[DeepWiki](https://deepwiki.com) answers a different question. It generates a
navigable wiki for a repository, which is useful for understanding an unfamiliar
codebase. It is not a package API reference, so it does not replace a docs
lookup when the problem is "which method exists in this version."

## When to choose what

- **Choose flare-docs** when the project is Rust, npm, or Python, and you want
  lookups that stay local, have no usage cap, and never pass through a third
  party.
- **Choose Context7** when you need a language or framework outside those three,
  or want one index spanning many ecosystems.
- **Choose Ref** when token budget matters more than local caching and your
  documentation is web-hosted.
- **Choose DeepWiki** when you are onboarding to a repository, not checking a
  library API.
- **Use the model's memory** only for stable, long-established APIs. It is the
  default, and it is exactly how renamed or removed methods get generated
  confidently.

## Sources

- [Context7](https://context7.com) · [context7 MCP server](https://github.com/upstash/context7)
- [Ref MCP](https://github.com/ref-tools/ref-tools-mcp)
- [DeepWiki](https://deepwiki.com)
- [docs.rs](https://docs.rs) · [npm registry](https://www.npmjs.com) · [PyPI](https://pypi.org)
