// Operator-authorized destructive refresh against the production keeper.
// A keeper hosting a live PTY is refused by default and, once force-live is
// authorized, is really stopped: the keeper process and its shell both exit.

import { afterAll, afterEach, expect, test } from "bun:test";
import { rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { cleanupLocalEndpoint } from "@roost/shared/local-endpoint";
import { shutdownKeeperForMaintenance } from "../src/keeper/update-admission.ts";
import { probeKeeperCompatible } from "../src/keeper/keeper-probe.ts";
import {
  MultiplexedKeeperPool,
  getMultiplexedPool,
  type MuxChannelCallbacks,
} from "../src/keeper/multiplexed-client.ts";
import { muxLocalEndpoint } from "../src/keeper/keeper-pool-config.ts";
import { keeperTestShellSpec } from "./keeper-test-fixtures.ts";

const TEST_ROOT = join(tmpdir(), `roost-test-keeper-force-live-${process.pid}`);
process.env.ROOST_WORKER_DATA_DIR = TEST_ROOT;
process.env.ROOST_KEEPER_QUIET = "1";

const ENDPOINT = muxLocalEndpoint();
const pools: MultiplexedKeeperPool[] = [];

function isAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

// Real subprocess, PTY, and process-exit bounds; fake timers cannot drive them.
async function waitUntil(
  predicate: () => boolean | Promise<boolean>,
  failure: string,
  timeoutMs: number = 10_000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await predicate()) return;
    await Bun.sleep(25);
  }
  throw new Error(failure);
}

afterEach(async () => {
  for (const pool of pools) pool.dispose();
  pools.length = 0;
  getMultiplexedPool().dispose();
  await cleanupLocalEndpoint(ENDPOINT);
});

afterAll(() => rmSync(TEST_ROOT, { recursive: true, force: true }));

test("force-live destroys a live keeper that the default refresh refuses", async () => {
  const marker = "ROOST_FORCE_LIVE_PTY";
  const channelId = 913;
  let output = "";
  const callbacks: MuxChannelCallbacks = {
    onOutput: (chunk) => { output += chunk.toString(); },
    onExit: () => {},
    onError: () => {},
  };
  const pool = new MultiplexedKeeperPool();
  pools.push(pool);
  const shellPid = await pool.spawn({
    channelId,
    shellSpec: keeperTestShellSpec({
      executable: "/bin/sh",
      argv: ["-c", "printf '%s\\n' \"$MARKER\"; exec /bin/sleep 60"],
      cwd: homedir(),
      env: { TERM: "xterm-256color", MARKER: marker },
    }),
    cols: 80,
    rows: 24,
    callbacks,
  });
  await waitUntil(() => output.includes(marker), "live PTY marker not observed");
  const before = await probeKeeperCompatible(ENDPOINT);
  expect(before.bindings).toEqual([{ channel_id: channelId, pid: shellPid }]);

  await expect(shutdownKeeperForMaintenance({ forceLive: false }))
    .rejects.toThrow("live channels");
  expect(isAlive(before.keeperPid!)).toBe(true);
  expect(isAlive(shellPid)).toBe(true);
  expect((await probeKeeperCompatible(ENDPOINT)).bindings)
    .toEqual([{ channel_id: channelId, pid: shellPid }]);

  expect(await shutdownKeeperForMaintenance({ forceLive: true })).toBe("shutdown");

  expect((await probeKeeperCompatible(ENDPOINT, 250)).reachable).toBe(false);
  await waitUntil(
    () => !isAlive(before.keeperPid!),
    "keeper process survived the authorized destruction",
  );
  await waitUntil(
    () => !isAlive(shellPid),
    "PTY shell survived the authorized destruction",
  );
}, 40_000);
