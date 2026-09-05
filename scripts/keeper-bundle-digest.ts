// Builds the canonical transitive keeper bundle used for implementation identity.
// scripts/gen-embed.ts persists the digest for source mode; build-binary.ts
// injects the same value into compiled release artifacts.

import { createHash } from "node:crypto";
import { dirname, join } from "node:path";

const REPOSITORY_ROOT = dirname(import.meta.dir);
const KEEPER_ENTRY = join(REPOSITORY_ROOT, "apps/worker/src/keeper/multiplexed-main.ts");
const GENERATED_CONTRACT = /keeper-contract\.generated\.ts$/;
const DIGEST_SENTINEL = "0".repeat(64);
const BUILD_SHA_SENTINEL = "0".repeat(40);

export async function buildKeeperImplementationDigest(): Promise<string> {
  const result = await Bun.build({
    entrypoints: [KEEPER_ENTRY],
    target: "bun",
    format: "esm",
    minify: true,
    sourcemap: "none",
    define: {
      __ROOST_KEEPER_IMPLEMENTATION_DIGEST__: JSON.stringify(DIGEST_SENTINEL),
      __ROOST_GIT_SHA__: JSON.stringify(BUILD_SHA_SENTINEL),
      __ROOST_VERSION__: JSON.stringify("keeper-bundle-v1"),
    },
    plugins: [{
      name: "keeper-contract-digest-sentinel",
      setup(builder) {
        builder.onLoad({ filter: GENERATED_CONTRACT }, () => ({
          contents: `export const GENERATED_KEEPER_IMPLEMENTATION_DIGEST = ${JSON.stringify(DIGEST_SENTINEL)};\n`,
          loader: "ts",
        }));
      },
    }],
  });
  if (!result.success) {
    const detail = result.logs.map(entry => String(entry)).join("; ");
    throw new Error(`keeper bundle generation failed${detail ? `: ${detail}` : ""}`);
  }
  if (result.outputs.length !== 1) {
    throw new Error(`keeper bundle generation produced ${result.outputs.length} outputs`);
  }
  const bytes = new Uint8Array(await result.outputs[0]!.arrayBuffer());
  return createHash("sha256").update(bytes).digest("hex");
}
