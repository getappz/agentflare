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
      description: "Documentation for agentflare's flare-docs — always-current package docs for your AI coding agent.",
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
          label: "flare-docs",
          items: [
            { label: "Overview", slug: "index" },
            { label: "Compare", slug: "compare" },
            { label: "Supported languages", slug: "how-it-works" },
            { label: "Using it from your agent", slug: "mcp-tool" },
            { label: "CLI reference", slug: "cli" },
            { label: "Examples", slug: "examples" },
          ],
        },
      ],
    }),
  ],
});
