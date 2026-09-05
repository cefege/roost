// Guards KeeperContractV1 generation, comparison, and authenticated Hello.
// The source contract digest must equal the canonical transitive keeper bundle,
// and a real detached keeper must report that exact contract and its bindings.

import { afterAll, describe, expect, test } from "bun:test";
import { rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { buildKeeperImplementationDigest } from "../../../scripts/keeper-bundle-digest.ts";
import {
  MultiplexedKeeperPool,
  probeKeeperCompatible,
  type MuxChannelCallbacks,
} from "../src/keeper/multiplexed-client.ts";
import { muxLocalEndpoint } from "../src/keeper/keeper-pool-config.ts";
import {
  KEEPER_TARGET_CONTRACT,
  keeperContractsExactlyEqual,
  keeperContractsSameImplementation,
} from "../src/keeper/keeper-stamp.ts";
import { keeperTestShellSpec } from "./keeper-test-fixtures.ts";

const SOCK_DIR = join(tmpdir(), `roost-test-keeper-contract-${process.pid}`);
process.env.ROOST_WORKER_DATA_DIR = SOCK_DIR;
process.env.ROOST_KEEPER_QUIET = "1";

const pool = new MultiplexedKeeperPool();
const callbacks: MuxChannelCallbacks = {
  onOutput: () => {},
  onExit: () => {},
  onError: () => {},
};

afterAll(() => {
  const keeperPid = pool._keeperProc?.pid;
  pool.dispose();
  if (keeperPid) {
    try { process.kill(keeperPid, "SIGKILL"); } catch { /* already dead */ }
  }
  rmSync(SOCK_DIR, { recursive: true, force: true });
});

describe("keeper contract generation and authenticated observation", () => {
  test("source contract uses the canonical transitive bundle digest", async () => {
    const implementationDigest = KEEPER_TARGET_CONTRACT.implementation_digest;
    if (implementationDigest === null) throw new Error("source keeper digest is unproven");
    expect(implementationDigest).toMatch(/^[0-9a-f]{64}$/);
    expect(await buildKeeperImplementationDigest()).toBe(implementationDigest);
    expect(KEEPER_TARGET_CONTRACT.supported_features)
      .toEqual([...KEEPER_TARGET_CONTRACT.supported_features].sort());
    expect(KEEPER_TARGET_CONTRACT.required_features)
      .toEqual([...KEEPER_TARGET_CONTRACT.required_features].sort());
  }, 15_000);

  test("missing digest is never exact or the same implementation", () => {
    const unproven = {
      ...KEEPER_TARGET_CONTRACT,
      implementation_digest: null,
    };
    expect(keeperContractsSameImplementation(
      KEEPER_TARGET_CONTRACT,
      unproven,
    )).toBe(false);
    expect(keeperContractsExactlyEqual(
      KEEPER_TARGET_CONTRACT,
      unproven,
    )).toBe(false);
  });

  test("build provenance does not change implementation equality", () => {
    const priorBuild = {
      ...KEEPER_TARGET_CONTRACT,
      build_sha: "prior-build",
    };
    expect(keeperContractsSameImplementation(
      KEEPER_TARGET_CONTRACT,
      priorBuild,
    )).toBe(true);
    expect(keeperContractsExactlyEqual(
      KEEPER_TARGET_CONTRACT,
      priorBuild,
    )).toBe(false);
  });

  test("real keeper reports exact contract, process epoch, and bindings", async () => {
    const channelId = 700;
    const shellPid = await pool.spawn({
      channelId,
      shellSpec: keeperTestShellSpec({
        executable: "/bin/sh",
        argv: ["-c", "exec sleep 60"],
        cwd: homedir(),
      }),
      cols: 80,
      rows: 24,
      callbacks,
    });
    const result = await probeKeeperCompatible(muxLocalEndpoint());
    expect(result).toMatchObject({
      reachable: true,
      authenticated: true,
      protocolCompatible: true,
      exactTarget: true,
      contract: KEEPER_TARGET_CONTRACT,
      bindings: [{ channel_id: channelId, pid: shellPid }],
      spawningChannels: [],
    });
    expect(result.keeperPid).toBeGreaterThan(0);
    expect(result.processEpoch).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
    );
    expect(pool.getRunningKeeperContract()).toEqual(KEEPER_TARGET_CONTRACT);
  }, 15_000);
});
