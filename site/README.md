# agentflare.dev

The landing site for agentflare, deployed to **Cloudflare Workers** with static
assets — the same setup pattern as the sarvo apps (`apps/web`: a Worker with an
`assets` binding + a custom-domain route).

```text
site/
├── wrangler.jsonc     # Worker config — assets binding + agentflare.dev custom domain
├── package.json       # dev / deploy / tail scripts (wrangler only, no build step)
├── src/
│   └── worker.ts      # serves ./public from ASSETS; proxies /install.sh + /install.ps1 → raw GitHub
└── public/
    ├── index.html     # the site — self-contained (inline CSS/JS, SVG favicon, self-hosted Commit Mono)
    ├── 404.html       # on-brand not-found page
    └── fonts/         # Commit Mono woff2 (400 + 700) + OFL LICENSE.txt
```

There is **no build step** — `public/index.html` is hand-authored and shipped as-is.

## Deploy

```bash
cd site
npm install          # or: aube install  — pulls wrangler
npm run deploy       # wrangler deploy
```

`wrangler deploy` uploads `public/` to the `ASSETS` binding, bundles `src/worker.ts`,
and (first deploy only) provisions the `agentflare.dev` + `www.agentflare.dev` custom
domains declared in `wrangler.jsonc`. The apex zone must already be on this Cloudflare
account; Wrangler creates the custom-domain records.

Local preview: `npm run dev` (serves on `localhost:8787`). Live logs: `npm run tail`.

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
