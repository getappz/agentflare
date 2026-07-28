# agentflare.dev

The landing site for agentflare, deployed to **Cloudflare Workers** with static
assets — the same setup pattern as the sarvo apps (`apps/web`: a Worker with an
`assets` binding + a custom-domain route).

Part of a small aube-linked monorepo rooted at the repo root (`/aube-workspace.yaml`,
`/package.json`, `/mise.toml`) alongside [`docs-site/`](../docs-site). One `aube install`
at the repo root installs both; each package still runs its own `dev`/`build` standalone.
See the repo root `mise.toml` for the task shortcuts (`mise run <task>`, `mise tasks ls`
to list them) and "Docs" below for how the two packages actually connect.

```text
site/
├── wrangler.jsonc     # Worker config — assets binding + agentflare.dev custom domain
├── package.json       # dev / build:docs / copy:docs / deploy / tail scripts
├── src/
│   └── worker.ts      # serves ./public from ASSETS; proxies /install.sh + /install.ps1 → raw GitHub
└── public/
    ├── index.html     # the site — self-contained (inline CSS/JS, SVG favicon, self-hosted Commit Mono)
    ├── 404.html       # on-brand not-found page
    ├── docs/           # copied from ../../docs-site/dist by `aube deploy` — not hand-edited
    └── fonts/         # Commit Mono woff2 (400 + 700) + OFL LICENSE.txt

docs-site/              # sibling workspace package, NOT nested in site/ — see "Docs" below
```

The landing page has **no build step** — `public/index.html` is hand-authored and shipped
as-is. `/docs` is the exception: its content is built by the separate `docs-site/`
package and copied into `public/docs/` only during `aube deploy`.

## Deploy

```bash
mise run install   # aube install, from the repo root — installs site/ + docs-site/
mise run deploy    # build docs-site, copy into public/docs, wrangler deploy
```

Equivalent without `mise` tasks, from `site/`: `aube install && aube deploy`.

`wrangler deploy` uploads `public/` to the `ASSETS` binding, bundles `src/worker.ts`,
and (first deploy only) provisions the `agentflare.dev` + `www.agentflare.dev` custom
domains declared in `wrangler.jsonc`. The apex zone must already be on this Cloudflare
account; Wrangler creates the custom-domain records.

Local preview: `mise run dev:site` (serves on `localhost:8787`). Live logs: `mise run tail`.

## Docs (`/docs`)

[`docs-site/`](../docs-site) is a sibling workspace package (`agentflare-docs` in
`aube-workspace.yaml`) — an [Astro Starlight](https://starlight.astro.build) project
documenting agentflare's `flare-docs` module. It's deliberately **not** nested under
`site/`: day to day, `site/`'s own `dev`/`build` never touch it, and vice versa — only
the shared root `aube install` links them. See `docs-site/README.md` for its standalone
workflow (`mise run dev:docs`, or `cd docs-site && aube dev`).

The two connect at exactly one point — deploy:

1. `aube run build:docs` builds `docs-site/` via the workspace filter
   (`aube run build --filter agentflare-docs`), producing `docs-site/dist/`.
2. `aube run copy:docs` copies that output into `public/docs/`.
3. `wrangler deploy` uploads `public/` (landing page + copied docs) as one static bundle,
   so `/docs` deploys from the same `public/` upload with no separate Worker or route.

`aube deploy` (or `mise run deploy`) runs all three. `public/docs/` is generated, not
hand-edited — add `/public/docs/` to `site/.gitignore` so the copied build output isn't
committed.

## What the Worker does

- **Everything** → served from `public/` via the `ASSETS` binding.
- **`/install.sh`, `/install.ps1`** → proxied from
  `raw.githubusercontent.com/getappz/agentflare/master`, edge-cached 5 min, so
  `curl -fsSL https://agentflare.dev/install.sh | sh` works and the hero command
  resolves on the domain instead of exposing a raw GitHub URL.
- **Unknown paths** → `public/404.html` (`not_found_handling: "404-page"`).

## Editing

All page content lives in `public/index.html` — copy, the benchmark numbers, and the
animated hero terminal are inline and commented. Keep the numbers in sync with the
repo's root `README.md` metrics table; the site deliberately mirrors its
"attributed, not blended" framing.

## Shipped extras
- **Font** — [Commit Mono](https://commitmono.com) (OFL-1.1) is self-hosted under
  `public/fonts/` (weights 400 + 700, `@font-face` + `<link rel="preload">`), served
  same-origin by the Worker so the page makes no third-party font request. License at
  `public/fonts/LICENSE.txt`.
- **Analytics** — Cloudflare Web Analytics is enabled on the zone (automatic, cookieless
  — no beacon code in the page). For explicit control instead, add the manual beacon
  `<script defer src="https://static.cloudflareinsights.com/beacon.min.js" data-cf-beacon='{"token":"…"}'></script>`
  to `<head>`.

- **Social image** — `public/og.png` (1200×630) is referenced via `og:image` /
  `twitter:image` for rich link unfurls on X / Slack / Discord. Source of truth is
  `og.svg`; regenerate with:
  ```bash
  cd site && npm install --no-save @resvg/resvg-js && node scripts/render-og.mjs
  ```
