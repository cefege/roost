#!/usr/bin/env bun
// Build the single self-contained `roost` binary:
//   vite build (SPA) → gen-embed (bake SPA + migrations) → bun build --compile
//   → restore the empty embed stubs so the working tree stays clean.
// The output binary embeds the Bun runtime, so it runs with no Bun installed.
import { $ } from "bun";
import { existsSync } from "node:fs";

const OUT = "dist/roost";

// Stamp the binary's version: package.json version + short git sha, baked via
// --define so `roost version` / `roost update` know what they are.
const pkg = await Bun.file("package.json").json();
const sha = (await $`git rev-parse --short HEAD`.nothrow().quiet().text()).trim();
const VERSION = sha ? `${pkg.version}+${sha}` : `${pkg.version}`;

if (!existsSync("apps/web/dist/index.html")) {
  console.log(">> vite build (apps/web)");
  await $`bun x vite build`.cwd("apps/web");
} else {
  console.log(">> vite build skipped (apps/web/dist present — delete to rebuild)");
}

console.log(">> gen-embed (baking SPA + migrations)");
await $`bun scripts/gen-embed.ts`;

console.log(`>> bun build --compile → ${OUT} (version ${VERSION})`);
const define = `__ROOST_VERSION__=${JSON.stringify(VERSION)}`;
await $`bun build --compile --define ${define} apps/roost-cli/src/main.ts --outfile ${OUT}`;

console.log(">> restore embed stubs");
await $`bun scripts/gen-embed.ts --stub`;

console.log(`\n✓ built ${OUT}`);
