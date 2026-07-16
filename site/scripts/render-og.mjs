// Rasterize og.svg -> public/og.png (1200x630) for social link unfurls.
// Social platforms (Slack/X/Discord) don't render SVG og:images, so we ship a PNG.
// Run: cd site && npx --yes --package=@resvg/resvg-js node scripts/render-og.mjs
import { readFileSync, writeFileSync } from "node:fs";
import { Resvg } from "@resvg/resvg-js";

const svg = readFileSync(new URL("../og.svg", import.meta.url));
const resvg = new Resvg(svg, {
  fitTo: { mode: "width", value: 1200 },
  background: "#0B0D12",
  font: { loadSystemFonts: true },
});
const png = resvg.render().asPng();
const out = new URL("../public/og.png", import.meta.url);
writeFileSync(out, png);
console.log(`wrote public/og.png (${png.length} bytes)`);
