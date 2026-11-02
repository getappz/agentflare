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
  redirects: {
    "/architecture": "/docs/reference/architecture/",
    "/cli": "/docs/reference/cli/",
    "/mcp-tools": "/docs/reference/mcp-tools/",
    "/getting-started": "/docs/get-started/",
    "/flare-docs": "/docs/concepts/flare-docs/",
  },
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
          label: "Get started",
          items: [
            { label: "Overview", slug: "get-started" },
            { label: "Installation", slug: "get-started/install" },
            { label: "Wire into your agent", slug: "get-started/wire-into-agent" },
          ],
        },
        {
          label: "Concepts",
          items: [
            { label: "Overview", slug: "concepts" },
            { label: "Optimization", slug: "concepts/optimization" },
            { label: "Coordination", slug: "concepts/coordination" },
            { label: "flare-docs", slug: "concepts/flare-docs" },
          ],
        },
        {
          label: "Guides",
          items: [
            { label: "Overview", slug: "guides" },
            { label: "Use flare-docs", slug: "guides/flare-docs" },
            { label: "Coordinate agents", slug: "guides/coordinate-agents" },
            { label: "Channel notifications", slug: "guides/notifications" },
          ],
        },
        {
          label: "Reference",
          items: [
            { label: "CLI", slug: "reference/cli" },
            { label: "MCP tools", slug: "reference/mcp-tools" },
            {
              label: "Architecture",
              items: [
                { label: "Start here", slug: "reference/architecture" },
                { label: "Overview and capabilities", slug: "reference/architecture/overview" },
                { label: "Installation and verification", slug: "reference/architecture/installation-and-verification" },
                { label: "Internals", slug: "reference/architecture/internals" },
                { label: "Checkout and ecosystem", slug: "reference/architecture/checkout-and-ecosystem" },
                { label: "Blueprint", slug: "reference/architecture/blueprint" },
              ],
            },
          ],
        },
        {
          label: "Compare",
          items: [
            { label: "How it compares", slug: "compare" },
            { label: "Documentation lookup", slug: "compare/docs-lookup" },
            { label: "Context and memory", slug: "compare/context-and-memory" },
            { label: "Coordination and orchestration", slug: "compare/coordination-and-orchestration" },
            { label: "Code review", slug: "compare/code-review" },
            { label: "Supporting tools", slug: "compare/supporting-tools" },
          ],
        },
      ],
    }),
  ],
});
