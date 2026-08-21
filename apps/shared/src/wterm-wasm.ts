// Locations of the roost-patched wterm wasm and its committed digest.
//
// apps/shared/wasm/wterm-roost.wasm is a build of @wterm/core 0.3.4's Zig core
// (upstream commit 4a73024d9f9003972f9efa6fe1a9086d1c90417b) with the two audited
// source deltas in scripts/wterm-0.3.4-roost.patch: src/scrollback.zig raises
// MAX_SCROLLBACK_LINES from 1k to 10k, and src/terminal.zig preserves and
// resizes both terminal grids while alternate screen is active.
// scripts/rebuild-wterm-wasm.sh reproduces it and rewrites the
// .sha256 sidecar; wterm-core-factory.ts refuses to load bytes that do not
// hash to that sidecar.
//
// Resolved relative to THIS module so the paths are correct wherever
// apps/shared is rsynced (deploy.ts ships apps/shared/ to every worker host).

import { fileURLToPath } from "node:url";
import { WTERM_WASM_EMBED, WTERM_WASM_SHA256_EMBED } from "./wterm-wasm-embed.generated.ts";

// Compiled `roost` binary → embedded file path (baked by scripts/gen-embed.ts).
// From source → resolved relative to this module (rsync deploys keep layout).
export const WTERM_ROOST_WASM_PATH = WTERM_WASM_EMBED
  ?? fileURLToPath(new URL("../wasm/wterm-roost.wasm", import.meta.url));

const WTERM_ROOST_WASM_SHA256_PATH = fileURLToPath(
  new URL("../wasm/wterm-roost.wasm.sha256", import.meta.url),
);

/** The `sha256sum` line committed beside the wasm, reduced to its digest.
 * Baked into the compiled binary alongside the wasm so both modes verify
 * against the same recorded value. Throws if the sidecar is missing or
 * malformed — an unverifiable core is a hard load failure, not a warning. */
export async function expectedRoostWasmSha256(): Promise<string> {
  const manifest = WTERM_WASM_SHA256_EMBED
    ?? await Bun.file(WTERM_ROOST_WASM_SHA256_PATH).text();
  const digest = manifest.trim().split(/\s+/, 1)[0] ?? "";
  if (!/^[0-9a-f]{64}$/.test(digest)) {
    throw new Error(
      `wterm wasm digest sidecar is malformed (${WTERM_ROOST_WASM_SHA256_PATH}): `
      + `expected a sha256sum line, got ${JSON.stringify(manifest.slice(0, 120))}`,
    );
  }
  return digest;
}
