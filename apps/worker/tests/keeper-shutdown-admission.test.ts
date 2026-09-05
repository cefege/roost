// Proves empty-keeper shutdown closes command admission before acknowledging.
// A concurrently authenticated client must not spawn a PTY after the empty
// proof and before the shutdown acknowledgement drains.

import { afterAll, afterEach, expect, test } from "bun:test";
import { rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { cleanupLocalEndpoint, localEndpointEnv } from "@roost/shared/local-endpoint";
import { connectKeeperAuthenticated, probeKeeperCompatible } from "../src/keeper/keeper-probe.ts";
import { muxLocalEndpoint } from "../src/keeper/keeper-pool-config.ts";
import {
  MuxFrameType,
  decodeMuxFrames,
  encodeMuxFrame,
  encodeSpawnRequest,
  type MuxFrame,
} from "../src/keeper/protocol.ts";
import { keeperTestShellSpec } from "./keeper-test-fixtures.ts";

const TEST_ROOT = join(tmpdir(), `roost-test-keeper-shutdown-${process.pid}`);
process.env.ROOST_WORKER_DATA_DIR = TEST_ROOT;
process.env.ROOST_KEEPER_QUIET = "1";
const ENDPOINT = muxLocalEndpoint();
const FIXTURE_ENTRY = fileURLToPath(new URL("./keeper-contract-fixture.ts", import.meta.url));
let fixture: Bun.Subprocess | null = null;

// Real subprocess/socket readiness cannot be advanced with fake timers.
async function waitUntil(
  predicate: () => boolean | Promise<boolean>,
  failure: string,
): Promise<void> {
  const deadline = Date.now() + 5_000;
  while (Date.now() < deadline) {
    if (await predicate()) return;
    await Bun.sleep(20);
  }
  throw new Error(failure);
}

async function startKeeperFixture(): Promise<void> {
  const environment = Object.fromEntries(
    Object.entries(process.env).filter(
      (entry): entry is [string, string] => entry[1] !== undefined,
    ),
  );
  fixture = Bun.spawn({
    cmd: [process.execPath, "run", FIXTURE_ENTRY, ENDPOINT.address, "incompatible"],
    env: { ...environment, ...localEndpointEnv(ENDPOINT, "ROOST_KEEPER") },
    stdio: ["ignore", "ignore", "inherit"],
  });
  await waitUntil(
    async () => (await probeKeeperCompatible(ENDPOINT, 200)).authenticated,
    "keeper fixture did not authenticate",
  );
}

function waitForShutdownAcknowledgement(
  socket: import("node:net").Socket,
  initial: Buffer,
): Promise<void> {
  const completion = Promise.withResolvers<void>();
  let received = initial;
  let settled = false;
  const finish = (error?: Error) => {
    if (settled) return;
    settled = true;
    socket.removeAllListeners();
    try { socket.destroy(); } catch { /* already closed */ }
    if (error) completion.reject(error);
    else completion.resolve();
  };
  socket.on("data", (chunk: Buffer | Uint8Array) => {
    received = Buffer.concat([received, Buffer.from(chunk)]);
    let frames: MuxFrame[];
    ({ frames, remaining: received } = decodeMuxFrames(received));
    if (frames.some(frame => frame.type === MuxFrameType.ShutdownIfEmptyAck)) finish();
  });
  socket.once("error", finish);
  socket.once("close", () => {
    if (received.length > 0) {
      const { frames } = decodeMuxFrames(received);
      if (frames.some(frame => frame.type === MuxFrameType.ShutdownIfEmptyAck)) {
        finish();
        return;
      }
    }
    finish(new Error("keeper closed without shutdown acknowledgement"));
  });
  return completion.promise;
}

function waitForSpawnDisposition(
  socket: import("node:net").Socket,
  initial: Buffer,
): Promise<"closed" | "spawned"> {
  const completion = Promise.withResolvers<"closed" | "spawned">();
  let received = initial;
  let settled = false;
  const finish = (outcome: "closed" | "spawned") => {
    if (settled) return;
    settled = true;
    socket.removeAllListeners();
    try { socket.destroy(); } catch { /* already closed */ }
    completion.resolve(outcome);
  };
  socket.on("data", (chunk: Buffer | Uint8Array) => {
    received = Buffer.concat([received, Buffer.from(chunk)]);
    let frames: MuxFrame[];
    ({ frames, remaining: received } = decodeMuxFrames(received));
    if (frames.some(frame => frame.type === MuxFrameType.SpawnAck)) finish("spawned");
  });
  socket.once("error", () => finish("closed"));
  socket.once("close", () => finish("closed"));
  return completion.promise;
}

afterEach(async () => {
  try { fixture?.kill("SIGKILL"); } catch { /* already exited */ }
  fixture = null;
  await cleanupLocalEndpoint(ENDPOINT);
});

afterAll(() => rmSync(TEST_ROOT, { recursive: true, force: true }));

test("ShutdownIfEmpty rejects a concurrent Spawn before acknowledging", async () => {
  await startKeeperFixture();
  const administrator = await connectKeeperAuthenticated(ENDPOINT);
  const spawner = await connectKeeperAuthenticated(ENDPOINT);
  if (!administrator.authenticated || !spawner.authenticated) {
    throw new Error("keeper clients did not authenticate");
  }
  const acknowledged = waitForShutdownAcknowledgement(
    administrator.socket,
    administrator.remaining,
  );
  const spawnDisposition = waitForSpawnDisposition(spawner.socket, spawner.remaining);

  administrator.socket.write(encodeMuxFrame(
    MuxFrameType.ShutdownIfEmpty,
    0,
    new Uint8Array(0),
  ));
  spawner.socket.write(encodeMuxFrame(
    MuxFrameType.Spawn,
    991,
    encodeSpawnRequest({
      channel_id: 991,
      cols: 80,
      rows: 24,
      shell_spec: keeperTestShellSpec({
        executable: "/bin/sh",
        argv: ["-c", "exec /bin/sleep 60"],
        cwd: homedir(),
      }),
    }),
  ));
  administrator.socket.resume();
  spawner.socket.resume();

  await acknowledged;
  expect(await spawnDisposition).toBe("closed");
  await waitUntil(
    async () => !(await probeKeeperCompatible(ENDPOINT, 100)).reachable,
    "keeper remained reachable after acknowledged shutdown",
  );
}, 15_000);
