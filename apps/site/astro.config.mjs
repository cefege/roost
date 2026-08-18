import { defineConfig } from "astro/config";
import sitemap from "@astrojs/sitemap";

// Canonical origin for canonical/OG/sitemap URLs. Single source of truth:
// override with ROOST_SITE_ORIGIN and rebuild when the hostname changes.
// Public origin is roosttt.com (edge Caddy behind the `roost` Cloudflare
// tunnel); the tailnet URL https://ovh1-8c32g.tail67850e.ts.net:4443 serves
// the same dist for private previews and is not the canonical host.
const SITE_ORIGIN = process.env.ROOST_SITE_ORIGIN ?? "https://roosttt.com";

export default defineConfig({
  site: SITE_ORIGIN,
  output: "static",
  build: { format: "directory" },
  integrations: [sitemap()],
});
