// Boot survivor identity proof. A slow Hello is retried and adopted, a Hello
// carrying an unknown future field still authenticates, an endpoint that never
// proves identity is refused as unproven — never as live sessions — and a
// keeper predating the contract Hello is refused rather than silently adopted.

import { afterAll, afterEach, expect, test } from "bun:test";
import { existsSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { cleanupLocalEndpoint } from "@roost/shared/local-endpoint";
import {
  KEEPER_IDENTITY_UNPROVEN_ERROR,
  KEEPER_REPLACEMENT_BLOCKED_ERROR,
  handleKeeperSurvivor,
} from "../src/boot-keeper.ts";
import { probeKeeperCompatible } from "../src/keeper/keeper-probe.ts";
import { getMultiplexedPool } from "../src/keeper/multiplexed-client.ts";
import { muxLocalEndpoint } from "../src/keeper/keeper-pool-config.ts";
import { KEEPER_TARGET_CONTRACT } from "../src/keeper/keeper-stamp.ts";
import {
  FAKE_KEEPER_EPOCH,
  FAKE_KEEPER_PID,
  incompatibleKeeperContract,
  startFakeKeeper,
  stopFakeKeepers,
  type FakeKeeperOptions,
} from "./keeper-fake-survivor.ts";

const TEST_ROOT = join(tmpdir(), `roost-test-keeper-identity-${process.pid}`);
process.env.ROOST_WORKER_DATA_DIR = TEST_ROOT;
process.env.ROOST_KEEPER_QUIET = "1";

const ENDPOINT = muxLocalEndpoint();
const SLOW_HELLO_MS = 1_500;

function startSurvivor(options: FakeKeeperOptions) {
  return startFakeKeeper(ENDPOINT, options);
}

async function survivorFailure(
  coordinatorOpenSessionIds: ReadonlySet<string>,
): Promise<string> {
  try {
    await handleKeeperSurvivor(coordinatorOpenSessionIds);
  } catch (error) {
    return error instanceof Error ? error.message : String(error);
  }
  throw new Error("keeper survivor admission unexpectedly succeeded");
}

afterEach(async () => {
  await stopFakeKeepers();
  getMultiplexedPool().dispose();
  await cleanupLocalEndpoint(ENDPOINT);
});

afterAll(() => rmSync(TEST_ROOT, { recursive: true, force: true }));

test("adopts a survivor that only answers Hello after a slow delay", async () => {
  const server = await startSurvivor({ helloDelayMs: SLOW_HELLO_MS });

  await handleKeeperSurvivor(new Set(["slow-hello-session"]));

  expect(getMultiplexedPool().getRunningKeeperContract())
    .toEqual(KEEPER_TARGET_CONTRACT);
  expect(server.listening).toBe(true);
  expect(existsSync(ENDPOINT.address)).toBe(true);
  const after = await probeKeeperCompatible(ENDPOINT, 3_000);
  expect(after.keeperPid).toBe(FAKE_KEEPER_PID);
  expect(after.processEpoch).toBe(FAKE_KEEPER_EPOCH);
}, 20_000);

test("authenticates a Hello carrying an unknown future field", async () => {
  await startSurvivor({ unknownField: true });

  const probe = await probeKeeperCompatible(ENDPOINT, 3_000);
  expect(probe.authenticated).toBe(true);
  expect(probe.protocolCompatible).toBe(true);
  expect(probe.keeperPid).toBe(FAKE_KEEPER_PID);
  expect(probe.bindings).toEqual([]);

  await handleKeeperSurvivor(new Set());
  expect(getMultiplexedPool().getRunningKeeperContract())
    .toEqual(KEEPER_TARGET_CONTRACT);
}, 20_000);

test("reports an endpoint that never proves identity as unproven, not busy", async () => {
  const server = await startSurvivor({ silent: true });

  const failure = await survivorFailure(new Set());

  expect(failure).toContain(KEEPER_IDENTITY_UNPROVEN_ERROR);
  expect(failure).not.toContain("live sessions");
  expect(server.listening).toBe(true);
  expect(existsSync(ENDPOINT.address)).toBe(true);
}, 30_000);

test("reports a proven keeper holding channels as blocked by live sessions", async () => {
  await startSurvivor({
    contract: incompatibleKeeperContract(),
    bindings: [{ channel_id: 31, pid: 9191 }],
  });

  const failure = await survivorFailure(new Set());

  expect(failure).toContain(KEEPER_REPLACEMENT_BLOCKED_ERROR);
  expect(failure).not.toContain(KEEPER_IDENTITY_UNPROVEN_ERROR);
}, 20_000);

test("refuses a keeper that authenticates without contract or binding proof", async () => {
  const server = await startSurvivor({ preContract: true });

  const probe = await probeKeeperCompatible(ENDPOINT, 3_000);
  expect(probe).toMatchObject({
    authenticated: true,
    protocolCompatible: false,
    keeperPid: FAKE_KEEPER_PID,
  });
  expect(probe.bindings).toBeUndefined();

  // Occupancy cannot rescue or worsen it: an unprovable binding set is refused
  // as unproven whether or not the coordinator holds open sessions.
  expect(await survivorFailure(new Set())).toContain(KEEPER_IDENTITY_UNPROVEN_ERROR);
  const withSessions = await survivorFailure(
    new Set(["00000000-0000-4000-8000-000000000009"]),
  );
  expect(withSessions).toContain(KEEPER_IDENTITY_UNPROVEN_ERROR);
  expect(withSessions).not.toContain(KEEPER_REPLACEMENT_BLOCKED_ERROR);
  expect(server.listening).toBe(true);
  expect(existsSync(ENDPOINT.address)).toBe(true);
}, 20_000);
