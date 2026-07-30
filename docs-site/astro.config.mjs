import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";

// Independent project, own dev workflow (mise + aube) — see README.md. Built
// with `base: "/docs"` because the output is only ever served mounted at
// agentflare.dev/docs; site/'s deploy step copies dist/ into site/public/docs
// right before `wrangler deploy`. Not built directly into site/ — that's the
// whole point of splitting this out.
export default defineConfig({
  site: "https://agentflare.dev",
  base: "/docs",
  integrations: [
    starlight({
      title: "agentflare docs",
      description: "Documentation for agentflare — optimize a single AI coding agent session and coordinate more than one of them.",
      logo: {
        src: "./src/assets/logo.svg",
        alt: "agentflare",
      },
      social: [{ icon: "github", label: "GitHub", href: "https://github.com/getappz/agentflare" }],
      customCss: ["./src/styles/custom.css"],
      // Starlight appends the page's full path from the project root itself
      // (src/content/docs/...) — baseUrl must stop at the project root.
      editLink: {
        baseUrl: "https://github.com/getappz/agentflare/edit/master/docs-site/",
      },
      sidebar: [
        {
          label: "Guide",
          items: [
            { label: "Overview", slug: "index" },
            { label: "Getting started", slug: "getting-started" },
            { label: "Concepts", slug: "concepts" },
            { label: "Guides", slug: "guides" },
          ],
        },
        {
          label: "Reference",
          items: [
            { label: "CLI reference", slug: "cli" },
            { label: "MCP tools reference", slug: "mcp-tools" },
            { label: "flare-docs", slug: "flare-docs" },
            { label: "How it compares", slug: "compare" },
          ],
        },
      ],
    }),
  ],
});
