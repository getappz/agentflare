# agentflare docs

Documentation for agentflare's `flare-docs` module, built with
[Astro Starlight](https://starlight.astro.build). A workspace member of the root
aube-workspace.yaml alongside `../site`, but a standalone project day to day — no
install/build coupling to `../site`; the two connect only at deploy time (see
`../site/README.md#docs-docs`).

## Develop

```bash
aube install    # or, from the repo root: mise run install
aube dev        # or: mise run dev:docs — localhost:4321/docs
```

## Build

```bash
aube build  # or: mise run build:docs — -> dist/
```

`base: "/docs"` in `astro.config.mjs` means the build only makes sense mounted at
`agentflare.dev/docs` — `aube preview` serves it that way locally too.

## Content

`src/content/docs/*.md` / `*.mdx`. Page order and labels are set in `astro.config.mjs`'s
`sidebar` config, not inferred from the filesystem.
