#!/usr/bin/env bun
// Build the self-contained `roost` binaries:
//   vite build (SPA) → gen-embed (bake SPA + migrations) → bun build --compile
//   → restore the empty embed stubs so the working tree stays clean.
// The output binaries embed the Bun runtime, so they run with no Bun installed.
//
// Default: the host-native `dist/roost` PLUS the full release matrix, so one
// macOS runner can publish every asset. Cross-compilation is safe here because
// there are no native modules anywhere — the PTY is Bun's native
// Bun.spawn({terminal}) and the DB is bun:sqlite. `--host-only` skips the
// matrix for fast local builds.
import { $ } from "bun";
import { existsSync } from "node:fs";

const OUT = "dist/roost";
// Asset names must match releaseAssetName() in apps/roost-cli/src/update.ts and
// the `case` in install-binary.sh, minus the legacy unsuffixed darwin-arm64
// `roost` which is emitted separately as the host build below.
const TARGETS = [
  { target: "bun-darwin-arm64", out: "dist/roost-darwin-arm64" },
  { target: "bun-darwin-x64", out: "dist/roost-darwin-x64" },
  { target: "bun-linux-x64", out: "dist/roost-linux-x64" },
  { target: "bun-linux-arm64", out: "dist/roost-linux-arm64" },
];
const hostOnly = process.argv.includes("--host-only");

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

try {
  console.log(">> gen-embed (baking SPA + migrations)");
  await $`bun scripts/gen-embed.ts`;

  const define = `__ROOST_VERSION__=${JSON.stringify(VERSION)}`;
  console.log(`>> bun build --compile → ${OUT} (host, version ${VERSION})`);
  await $`bun build --compile --define ${define} apps/roost-cli/src/main.ts --outfile ${OUT}`;

  if (!hostOnly) {
    for (const t of TARGETS) {
      console.log(`>> bun build --compile --target=${t.target} → ${t.out}`);
      await $`bun build --compile --target=${t.target} --define ${define} apps/roost-cli/src/main.ts --outfile ${t.out}`;
    }
  }
} finally {
  console.log(">> restore embed stubs");
  await $`bun scripts/gen-embed.ts --stub`;
}

console.log(`\n✓ built ${OUT}${hostOnly ? "" : ` + ${TARGETS.map((t) => t.out).join(", ")}`}`);
