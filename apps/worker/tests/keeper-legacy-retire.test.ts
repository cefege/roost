// Operator-authorized retirement of a keeper that predates binding proof.
// Such a survivor authenticates but cannot describe its channels, so the worker
// can neither adopt nor automatically replace it: without the authorization
// boot stays refused, and the authorization never reaches a survivor that
// proves its bindings or one that never proved keeper identity at all.

import { afterAll, afterEach, expect, test } from "bun:test";
import { rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { cleanupLocalEndpoint } from "@roost/shared/local-endpoint";
import {
  KEEPER_IDENTITY_UNPROVEN_ERROR,
  KEEPER_REPLACEMENT_BLOCKED_ERROR,
  handleKeeperSurvivor,
} from "../src/boot-keeper.ts";
import { muxLocalEndpoint } from "../src/keeper/keeper-pool-config.ts";
import { probeKeeperCompatible } from "../src/keeper/keeper-probe.ts";
import {
  incompatibleKeeperContract,
  startFakeKeeper,
  stopFakeKeepers,
} from "./keeper-fake-survivor.ts";

const TEST_ROOT = join(tmpdir(), `roost-test-keeper-legacy-retire-${process.pid}`);
process.env.ROOST_WORKER_DATA_DIR = TEST_ROOT;
process.env.ROOST_KEEPER_QUIET = "1";

const ENDPOINT = muxLocalEndpoint();
const LIVE_SESSION = "60000000-0000-4000-8000-000000000006";

async function survivorFailure(
  forceLiveRetire: boolean,
  coordinatorOpenSessionIds: ReadonlySet<string>,
): Promise<string> {
  try {
    await handleKeeperSurvivor(coordinatorOpenSessionIds, forceLiveRetire);
  } catch (error) {
    return error instanceof Error ? error.message : String(error);
  }
  return "";
}

afterEach(async () => {
  await stopFakeKeepers();
  await cleanupLocalEndpoint(ENDPOINT);
});

afterAll(() => rmSync(TEST_ROOT, { recursive: true, force: true }));

test("force-live retires a survivor that predates binding proof", async () => {
  await startFakeKeeper(ENDPOINT, { preContract: true, retiresOnShutdown: true });
  const before = await probeKeeperCompatible(ENDPOINT);
  expect(before.authenticated).toBe(true);
  expect(before.protocolCompatible).toBe(false);
  expect(before.bindings).toBeUndefined();

  await handleKeeperSurvivor(new Set([LIVE_SESSION]), true);

  expect((await probeKeeperCompatible(ENDPOINT, 250)).reachable).toBe(false);
}, 20_000);

test("the same survivor keeps refusing boot without the authorization", async () => {
  await startFakeKeeper(ENDPOINT, { preContract: true, retiresOnShutdown: true });

  expect(await survivorFailure(false, new Set([LIVE_SESSION])))
    .toBe(KEEPER_IDENTITY_UNPROVEN_ERROR);
  expect((await probeKeeperCompatible(ENDPOINT)).authenticated).toBe(true);
}, 20_000);

test("force-live never retires a survivor that proves its bindings", async () => {
  await startFakeKeeper(ENDPOINT, {
    contract: incompatibleKeeperContract(),
    bindings: [{ channel_id: 7, pid: 4242 }],
    retiresOnShutdown: true,
  });

  expect(await survivorFailure(true, new Set([LIVE_SESSION])))
    .toBe(KEEPER_REPLACEMENT_BLOCKED_ERROR);
  expect((await probeKeeperCompatible(ENDPOINT)).authenticated).toBe(true);
}, 20_000);

test("force-live never retires a process that proved no keeper identity", async () => {
  await startFakeKeeper(ENDPOINT, { silent: true, retiresOnShutdown: true });

  expect(await survivorFailure(true, new Set([LIVE_SESSION])))
    .toBe(KEEPER_IDENTITY_UNPROVEN_ERROR);
  expect((await probeKeeperCompatible(ENDPOINT, 250)).reachable).toBe(true);
}, 30_000);
