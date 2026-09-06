// Keeper admission must describe the selected source checkout, not the CLI
// process performing the deployment. These tests pin that source boundary and
// the host-specific runtime overlay applied after the source probe.

import { afterEach, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  loadSourceKeeperContract,
  sourceKeeperContractCommand,
  targetKeeperContractForWorker,
} from "../src/push-keeper-admission.ts";

const roots: string[] = [];
const SOURCE_CONTRACT = {
  protocol_version: 37,
  supported_features: ["selected-source-feature"],
  required_features: ["selected-source-feature"],
  implementation_digest: "d".repeat(64),
  bun_abi: "source-bun",
  platform: "linux" as const,
  arch: "source-arch",
  build_sha: "source-build",
};

afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

describe("selected source keeper contract", () => {
  test("loads the contract by executing the selected checkout", async () => {
    const root = mkdtempSync(join(tmpdir(), "roost-source-contract-"));
    roots.push(root);
    const cliDirectory = join(root, "apps", "roost-cli", "src");
    mkdirSync(cliDirectory, { recursive: true });
    writeFileSync(
      join(cliDirectory, "main.ts"),
      `process.stdout.write(${JSON.stringify(JSON.stringify(SOURCE_CONTRACT))});`,
    );

    await expect(loadSourceKeeperContract(root)).resolves.toEqual(SOURCE_CONTRACT);
  });

  test("preserves selected implementation metadata while overlaying target runtime", () => {
    expect(targetKeeperContractForWorker(SOURCE_CONTRACT, "a".repeat(40), {
      bun_abi: "target-bun",
      platform: "darwin",
      arch: "arm64",
    })).toEqual({
      ...SOURCE_CONTRACT,
      bun_abi: "target-bun",
      platform: "darwin",
      arch: "arm64",
      build_sha: "a".repeat(40),
    });
  });

  test("uses PATH Bun instead of a compiled roost process executable", () => {
    expect(sourceKeeperContractCommand(
      "/selected",
      () => "/usr/local/bin/bun",
      "/opt/roost/bin/roost",
    )).toEqual([
      "/usr/local/bin/bun",
      "/selected/apps/roost-cli/src/main.ts",
      "__keeper-contract",
    ]);
  });
});
