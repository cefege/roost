import { defineConfig } from "astro/config";
import sitemap from "@astrojs/sitemap";

// Canonical origin for canonical/OG/sitemap URLs. Single source of truth:
// override with ROOST_SITE_ORIGIN and rebuild when the hostname changes.
const SITE_ORIGIN = process.env.ROOST_SITE_ORIGIN ?? "https://ovh1-8c32g.tail67850e.ts.net:4443";

export default defineConfig({
  site: SITE_ORIGIN,
  output: "static",
  build: { format: "directory" },
  integrations: [sitemap()],
});
