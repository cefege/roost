// Operator-authorized retirement of a keeper that predates binding proof.
// Such a survivor authenticates but cannot describe its channels, so the worker
// can neither adopt nor automatically replace it: without the authorization
// boot stays refused, and the authorization never reaches a survivor that
// proves its bindings or one that never proved keeper identity at all. An
// authorized boot also names what it destroys first and spends its own license.

import { afterAll, afterEach, expect, test } from "bun:test";
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
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
import { loadWorkerConfig } from "../src/config.ts";
import {
  KEEPER_FORCE_LIVE_RETIRE_ENV,
  spendKeeperForceLiveRetireAuthorization,
} from "../src/service-definition-env.ts";
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

/** The LaunchAgent a `roost deploy --force-live` activation starts the worker
 * from: the authorization is an installed entry, not an ambient variable. */
function authorizedServiceDefinition(): string {
  mkdirSync(TEST_ROOT, { recursive: true });
  const path = join(TEST_ROOT, "worker-forced.plist");
  writeFileSync(path, [
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
    "<plist version=\"1.0\"><dict><key>EnvironmentVariables</key><dict>",
    "<key>ROOST_COORDINATOR_URL</key><string>https://coord.example.test:4102</string>",
    `<key>${KEEPER_FORCE_LIVE_RETIRE_ENV}</key><string>1</string>`,
    "</dict></dict></plist>",
  ].join("\n"), { mode: 0o600 });
  return path;
}

/** warn/error go to console.error, so the emitted lines are the log. */
async function bootLogLines(boot: () => Promise<void>): Promise<string[]> {
  const lines: string[] = [];
  const originalError = console.error;
  console.error = (...values: unknown[]) => { lines.push(values.map(String).join(" ")); };
  try {
    await boot();
  } finally {
    console.error = originalError;
  }
  return lines;
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

test("an authorized boot retires the survivor and spends its own authorization", async () => {
  await startFakeKeeper(ENDPOINT, { preContract: true, retiresOnShutdown: true });
  const definitionPath = authorizedServiceDefinition();
  const cfg = loadWorkerConfig({ [KEEPER_FORCE_LIVE_RETIRE_ENV]: "1" }, "darwin");
  expect(cfg.keeperForceLiveRetire).toBe(true);

  const lines = await bootLogLines(async () => {
    await spendKeeperForceLiveRetireAuthorization(definitionPath, "darwin");
    await handleKeeperSurvivor(new Set([LIVE_SESSION]), cfg.keeperForceLiveRetire);
  });

  expect((await probeKeeperCompatible(ENDPOINT, 250)).reachable).toBe(false);
  const discarding = lines.findIndex((line) => line.includes("keeper_force_live_retire_discarding"));
  const retired = lines.findIndex((line) => line.includes("keeper_force_live_retired"));
  expect(discarding).toBeGreaterThanOrEqual(0);
  expect(retired).toBeGreaterThan(discarding);
  const discarded = JSON.parse(lines[discarding]!) as Record<string, unknown>;
  expect(discarded.coordinator_session_ids).toEqual([LIVE_SESSION]);
  expect(discarded.keeper_binding_channel_ids).toBeNull();
  expect(lines.some((line) => line.includes("keeper_force_live_retire_authorization_spent")))
    .toBe(true);
  const spent = readFileSync(definitionPath, "utf8");
  expect(spent).not.toContain(KEEPER_FORCE_LIVE_RETIRE_ENV);
  expect(spent).toContain("ROOST_COORDINATOR_URL");
  expect(loadWorkerConfig({}, "darwin").keeperForceLiveRetire).toBe(false);
}, 20_000);

test("force-live names the channel bindings it destroys before ending them", async () => {
  await startFakeKeeper(ENDPOINT, {
    contract: incompatibleKeeperContract(),
    bindings: [{ channel_id: 7, pid: 4242 }],
    omitSpawningChannels: true,
    retiresOnShutdown: true,
  });

  const lines = await bootLogLines(async () => {
    await handleKeeperSurvivor(new Set([LIVE_SESSION]), true);
  });

  const discarding = lines.findIndex((line) => line.includes("keeper_force_live_retire_discarding"));
  expect(discarding).toBeGreaterThanOrEqual(0);
  const discarded = JSON.parse(lines[discarding]!) as Record<string, unknown>;
  expect(discarded.keeper_binding_channel_ids).toEqual([7]);
  expect(discarded.spawning_channels).toBeNull();
  expect((await probeKeeperCompatible(ENDPOINT, 250)).reachable).toBe(false);
}, 20_000);
