// Boot survivor identity proof. A slow Hello is retried and adopted, a Hello
// carrying an unknown future field still authenticates, and an endpoint that
// never proves identity is refused as unproven — never as live sessions.

import { afterAll, afterEach, expect, test } from "bun:test";
import { existsSync, rmSync } from "node:fs";
import { createServer, type Server, type Socket } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  cleanupLocalEndpoint,
  prepareLocalEndpoint,
} from "@roost/shared/local-endpoint";
import type { KeeperContractV1 } from "@roost/shared/keeper-update";
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
  KEEPER_PROTOCOL_VERSION,
  MuxFrameType,
  decodeMuxFrames,
  encodeMuxFrame,
} from "../src/keeper/protocol.ts";

const TEST_ROOT = join(tmpdir(), `roost-test-keeper-identity-${process.pid}`);
process.env.ROOST_WORKER_DATA_DIR = TEST_ROOT;
process.env.ROOST_KEEPER_QUIET = "1";

const ENDPOINT = muxLocalEndpoint();
const FAKE_KEEPER_PID = 424242;
const FAKE_KEEPER_EPOCH = "50000000-0000-4000-8000-000000000005";
const SLOW_HELLO_MS = 1_500;

interface FakeKeeperOptions {
  /** Milliseconds between receiving Hello and answering it. */
  helloDelayMs?: number;
  /** Never answer Hello: the endpoint accepts but proves nothing. */
  silent?: boolean;
  /** A field this build has never heard of, as a newer keeper would send. */
  unknownField?: boolean;
  contract?: KeeperContractV1;
  bindings?: readonly { channel_id: number; pid: number }[];
}

const servers: Server[] = [];
const openSockets = new Set<Socket>();

function helloResponseBytes(options: FakeKeeperOptions): Uint8Array {
  const response: Record<string, unknown> = {
    version: KEEPER_PROTOCOL_VERSION,
    authenticated: true,
    features: [...KEEPER_TARGET_CONTRACT.supported_features],
    contract: options.contract ?? KEEPER_TARGET_CONTRACT,
    pid: FAKE_KEEPER_PID,
    process_epoch: FAKE_KEEPER_EPOCH,
    bindings: options.bindings ?? [],
    spawning_channels: [],
  };
  if (options.unknownField) {
    response.keeper_future_capability = { generation: 2, notes: "unknown" };
  }
  return Buffer.from(JSON.stringify(response), "utf8");
}

// The delay under test is real socket latency: the retry loop reads the wall
// clock and reacts to kernel socket events, which fake timers cannot drive.
async function startFakeKeeper(options: FakeKeeperOptions): Promise<Server> {
  await prepareLocalEndpoint(ENDPOINT);
  const server = createServer((socket) => {
    openSockets.add(socket);
    socket.on("close", () => openSockets.delete(socket));
    socket.on("error", () => { /* client hangs up on probe timeout */ });
    let received = Buffer.alloc(0) as Buffer;
    socket.on("data", (chunk: Buffer) => {
      received = Buffer.concat([received, Buffer.from(chunk)]);
      const { frames, remaining } = decodeMuxFrames(received);
      received = remaining;
      for (const frame of frames) {
        if (frame.type !== MuxFrameType.Hello || options.silent) continue;
        setTimeout(() => {
          if (socket.destroyed) return;
          socket.write(encodeMuxFrame(
            MuxFrameType.HelloResp,
            0,
            helloResponseBytes(options),
          ));
        }, options.helloDelayMs ?? 0);
      }
    });
  });
  servers.push(server);
  const listening = Promise.withResolvers<void>();
  server.once("error", listening.reject);
  server.listen(ENDPOINT.address, () => listening.resolve());
  await listening.promise;
  return server;
}

function incompatibleContract(): KeeperContractV1 {
  return {
    ...KEEPER_TARGET_CONTRACT,
    required_features: [
      ...KEEPER_TARGET_CONTRACT.required_features,
      "keeper_requirement_this_worker_lacks_v1",
    ].sort(),
    build_sha: "incompatible-identity-fixture",
  };
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
  for (const socket of openSockets) socket.destroy();
  openSockets.clear();
  for (const server of servers) {
    const closed = Promise.withResolvers<void>();
    server.close(() => closed.resolve());
    await closed.promise;
  }
  servers.length = 0;
  getMultiplexedPool().dispose();
  await cleanupLocalEndpoint(ENDPOINT);
});

afterAll(() => rmSync(TEST_ROOT, { recursive: true, force: true }));

test("adopts a survivor that only answers Hello after a slow delay", async () => {
  const server = await startFakeKeeper({ helloDelayMs: SLOW_HELLO_MS });

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
  await startFakeKeeper({ unknownField: true });

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
  const server = await startFakeKeeper({ silent: true });

  const failure = await survivorFailure(new Set());

  expect(failure).toContain(KEEPER_IDENTITY_UNPROVEN_ERROR);
  expect(failure).not.toContain("live sessions");
  expect(server.listening).toBe(true);
  expect(existsSync(ENDPOINT.address)).toBe(true);
}, 30_000);

test("reports a proven keeper holding channels as blocked by live sessions", async () => {
  await startFakeKeeper({
    contract: incompatibleContract(),
    bindings: [{ channel_id: 31, pid: 9191 }],
  });

  const failure = await survivorFailure(new Set());

  expect(failure).toContain(KEEPER_REPLACEMENT_BLOCKED_ERROR);
  expect(failure).not.toContain(KEEPER_IDENTITY_UNPROVEN_ERROR);
}, 20_000);
