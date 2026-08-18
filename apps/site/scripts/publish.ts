#!/usr/bin/env bun
// Publish the built site to the path the edge Caddy container serves.
//
// The edge (`caddy` in /srv/infra/edge/docker-compose.yml) mounts
// PUBLISH_ROOT read-only and file_servers it for roosttt.com, exactly like
// /srv/verdeka/www. Publishing is therefore an rsync of dist/ — no container
// restart, no cache to purge beyond Cloudflare's.
//
// Build first so canonical/OG/sitemap URLs match the public origin:
//   ROOST_SITE_ORIGIN=https://roosttt.com bun run build && bun scripts/publish.ts
import { existsSync } from "node:fs";
import { resolve } from "node:path";

const DIST = resolve(import.meta.dir, "..", "dist");
const PUBLISH_ROOT = process.env.ROOST_SITE_PUBLISH_ROOT ?? "/srv/roost-site/www";

if (!existsSync(`${DIST}/index.html`)) {
  console.error(`publish: no build at ${DIST} — run \`bun run build\` first`);
  process.exit(1);
}

// --delete keeps the served tree byte-identical to dist (stale hashed assets
// in /_astro would otherwise accumulate forever).
const rsync = Bun.spawnSync(["rsync", "-a", "--delete", `${DIST}/`, `${PUBLISH_ROOT}/`], {
  stdout: "inherit",
  stderr: "inherit",
});
if (rsync.exitCode !== 0) {
  console.error(`publish: rsync exited ${rsync.exitCode}`);
  process.exit(rsync.exitCode ?? 1);
}

const pages = [...new Bun.Glob("**/*.html").scanSync(PUBLISH_ROOT)].length;
console.log(`publish: ${pages} pages -> ${PUBLISH_ROOT}`);
