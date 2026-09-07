// Exercises fail-closed keeper survivor admission against real keeper processes.
// Compatible live PTYs must survive adoption; protocol-incompatible keepers may
// be replaced only when coordinator sessions and keeper bindings prove empty.

import { afterAll, afterEach, describe, expect, test } from "bun:test";
import { existsSync, rmSync } from "node:fs";
import { createServer, type Server, type Socket } from "node:net";
import { homedir, tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import {
  cleanupLocalEndpoint,
  localEndpointEnv,
  prepareLocalEndpoint,
} from "@roost/shared/local-endpoint";
import {
  KEEPER_IDENTITY_UNPROVEN_ERROR,
  KEEPER_REPLACEMENT_BLOCKED_ERROR,
  handleKeeperSurvivor,
} from "../src/boot-keeper.ts";
import {
  MultiplexedKeeperPool,
  getMultiplexedPool,
  probeKeeperCompatible,
  shutdownKeeperAuthenticated,
  type MuxChannelCallbacks,
} from "../src/keeper/multiplexed-client.ts";
import { connectKeeperAuthenticated } from "../src/keeper/keeper-probe.ts";
import { muxLocalEndpoint } from "../src/keeper/keeper-pool-config.ts";
import {
  MuxFrameType,
  decodeMuxFrames,
  encodeMuxFrame,
  encodeSpawnRequest,
  type MuxFrame,
} from "../src/keeper/protocol.ts";
import { keeperTestShellSpec } from "./keeper-test-fixtures.ts";

const TEST_ROOT = join(tmpdir(), `roost-test-keeper-survivor-${process.pid}`);
process.env.ROOST_WORKER_DATA_DIR = TEST_ROOT;
process.env.ROOST_KEEPER_QUIET = "1";

const ENDPOINT = muxLocalEndpoint();
const FIXTURE_ENTRY = fileURLToPath(new URL(
  "./keeper-contract-fixture.ts",
  import.meta.url,
));
const pools: MultiplexedKeeperPool[] = [];
const fixtureProcesses: Bun.Subprocess[] = [];

function isAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

// These are real subprocess/socket readiness bounds; fake timers cannot drive
// kernel listener, PTY, or process-exit events.
async function waitUntil(
  predicate: () => boolean | Promise<boolean>,
  failure: string,
  timeoutMs: number = 5_000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await predicate()) return;
    await Bun.sleep(25);
  }
  throw new Error(failure);
}

function fixtureEnvironment(): Record<string, string> {
  const environment = Object.fromEntries(
    Object.entries(process.env).filter(
      (entry): entry is [string, string] => entry[1] !== undefined,
    ),
  );
  return {
    ...environment,
    ...localEndpointEnv(ENDPOINT, "ROOST_KEEPER"),
    ROOST_KEEPER_QUIET: "1",
  };
}

async function startContractFixture(
  mode: "compatible" | "incompatible",
): Promise<Bun.Subprocess> {
  const subprocess = Bun.spawn({
    cmd: [process.execPath, "run", FIXTURE_ENTRY, ENDPOINT.address, mode],
    env: fixtureEnvironment(),
    stdio: ["ignore", "ignore", "inherit"],
  });
  fixtureProcesses.push(subprocess);
  await waitUntil(async () => {
    const probe = await probeKeeperCompatible(ENDPOINT, 250);
    return probe.authenticated;
  }, `${mode} keeper fixture did not authenticate`);
  return subprocess;
}

function shellSpec(marker: string) {
  return keeperTestShellSpec({
    executable: "/bin/sh",
    argv: ["-c", "printf '%s\\n' \"$MARKER\"; exec /bin/sleep 60"],
    cwd: homedir(),
    env: { TERM: "xterm-256color", MARKER: marker },
  });
}

async function spawnThroughIncompatibleConnection(
  channelId: number,
  marker: string,
): Promise<{ shellPid: number; observedOutput: string }> {
  const attempt = await connectKeeperAuthenticated(ENDPOINT);
  if (!attempt.authenticated) throw new Error("keeper fixture did not authenticate");
  const socket = attempt.socket;
  const completion =
    Promise.withResolvers<{ shellPid: number; observedOutput: string }>();
  let received = attempt.remaining;
  let shellPid: number | null = null;
  let observedOutput = "";
  let settled = false;
  let timer: ReturnType<typeof setTimeout>;
  const finish = (error?: Error) => {
    if (settled) return;
    settled = true;
    clearTimeout(timer);
    socket.removeListener("data", onData);
    socket.removeListener("close", onClose);
    socket.removeListener("error", onError);
    try { socket.destroy(); } catch { /* already closed */ }
    if (error) completion.reject(error);
    else completion.resolve({ shellPid: shellPid!, observedOutput });
  };
  const inspectFrames = (frames: readonly MuxFrame[]) => {
    for (const frame of frames) {
      if (frame.channelId !== channelId) continue;
      if (frame.type === MuxFrameType.SpawnAck) {
        const parsed = JSON.parse(frame.payload.toString()) as { pid?: unknown };
        if (typeof parsed.pid === "number") shellPid = parsed.pid;
      } else if (frame.type === MuxFrameType.PtyOut) {
        observedOutput += frame.payload.toString();
      } else if (frame.type === MuxFrameType.SpawnErr) {
        finish(new Error(`fixture spawn failed: ${frame.payload.toString()}`));
        return;
      }
    }
    if (shellPid !== null && observedOutput.includes(marker)) finish();
  };
  const onData = (chunk: Buffer | Uint8Array) => {
    received = Buffer.concat([received, Buffer.from(chunk)]);
    let frames: MuxFrame[];
    ({ frames, remaining: received } = decodeMuxFrames(received));
    inspectFrames(frames);
  };
  const onClose = () => finish(new Error("keeper closed during fixture spawn"));
  const onError = (error: Error) => finish(error);
  timer = setTimeout(
    () => finish(new Error("fixture spawn timed out")),
    5_000,
  );
  socket.on("data", onData);
  socket.once("close", onClose);
  socket.once("error", onError);
  socket.write(encodeMuxFrame(
    MuxFrameType.Spawn,
    channelId,
    encodeSpawnRequest({
      channel_id: channelId,
      cols: 80,
      rows: 24,
      shell_spec: shellSpec(marker),
    }),
  ));
  socket.resume();
  return completion.promise;
}

async function readRawHistory(channelId: number): Promise<string> {
  const attempt = await connectKeeperAuthenticated(ENDPOINT);
  if (!attempt.authenticated) throw new Error("keeper fixture did not authenticate");
  const socket = attempt.socket;
  const completion = Promise.withResolvers<string>();
  let received = attempt.remaining;
  let settled = false;
  let timer: ReturnType<typeof setTimeout>;
  const finish = (history?: string, error?: Error) => {
    if (settled) return;
    settled = true;
    clearTimeout(timer);
    socket.removeListener("data", onData);
    socket.removeListener("close", onClose);
    socket.removeListener("error", onError);
    try { socket.destroy(); } catch { /* already closed */ }
    if (error) completion.reject(error);
    else completion.resolve(history!);
  };
  const onData = (chunk: Buffer | Uint8Array) => {
    received = Buffer.concat([received, Buffer.from(chunk)]);
    let frames: MuxFrame[];
    ({ frames, remaining: received } = decodeMuxFrames(received));
    for (const frame of frames) {
      if (
        frame.type === MuxFrameType.GetHistoryResp
        && frame.channelId === channelId
      ) {
        finish(frame.payload.subarray(8).toString());
        return;
      }
    }
  };
  const onClose = () =>
    finish(undefined, new Error("keeper closed during history read"));
  const onError = (error: Error) => finish(undefined, error);
  timer = setTimeout(
    () => finish(undefined, new Error("keeper history read timed out")),
    5_000,
  );
  socket.on("data", onData);
  socket.once("close", onClose);
  socket.once("error", onError);
  socket.write(encodeMuxFrame(
    MuxFrameType.GetHistory,
    channelId,
    new Uint8Array(0),
  ));
  socket.resume();
  return completion.promise;
}

async function closePreAuthServer(
  server: Server,
  sockets: ReadonlySet<Socket>,
): Promise<void> {
  for (const socket of sockets) socket.destroy();
  const closed = Promise.withResolvers<void>();
  server.close(() => closed.resolve());
  await closed.promise;
}

afterEach(async () => {
  for (const pool of pools) {
    try { pool.socket?.destroy(); } catch { /* already closed */ }
  }
  try { await shutdownKeeperAuthenticated(ENDPOINT, 1_000); } catch { /* absent */ }
  for (const subprocess of fixtureProcesses) {
    try { subprocess.kill("SIGKILL"); } catch { /* already exited */ }
  }
  for (const pool of pools) pool.dispose();
  pools.length = 0;
  fixtureProcesses.length = 0;
  getMultiplexedPool().dispose();
  await cleanupLocalEndpoint(ENDPOINT);
});

afterAll(() => {
  rmSync(TEST_ROOT, { recursive: true, force: true });
});

describe("keeper survivor continuity", () => {
  test("adopts a protocol-compatible live keeper without replacing its PTY", async () => {
    await startContractFixture("compatible");
    const marker = "ROOST_COMPATIBLE_SURVIVOR";
    const channelId = 811;
    let output = "";
    const callbacks: MuxChannelCallbacks = {
      onOutput: chunk => { output += chunk.toString(); },
      onExit: () => {},
      onError: () => {},
    };
    const originalPool = new MultiplexedKeeperPool();
    pools.push(originalPool);
    const shellPid = await originalPool.spawn({
      channelId,
      shellSpec: shellSpec(marker),
      cols: 80,
      rows: 24,
      callbacks,
    });
    await waitUntil(() => output.includes(marker), "compatible marker not observed");
    const before = await probeKeeperCompatible(ENDPOINT);
    expect(before.protocolCompatible).toBe(true);
    expect(before.exactTarget).toBe(false);
    originalPool.socket?.destroy();
    await waitUntil(() => originalPool.socket === null, "original worker socket stayed open");

    await handleKeeperSurvivor(new Set(["compatible-session"]));

    const after = await probeKeeperCompatible(ENDPOINT);
    expect(after.keeperPid).toBe(before.keeperPid);
    expect(after.processEpoch).toBe(before.processEpoch);
    expect(after.bindings).toEqual([{ channel_id: channelId, pid: shellPid }]);
    expect(isAlive(shellPid)).toBe(true);
    const adoptedPool = new MultiplexedKeeperPool();
    pools.push(adoptedPool);
    await adoptedPool.ensure();
    expect(await adoptedPool.listChannels()).toContainEqual({ channelId, pid: shellPid });
    expect(Buffer.from((await adoptedPool.getHistory(channelId)).bytes).toString())
      .toContain(marker);
  }, 20_000);

  test("blocks incompatible live replacement without changing keeper, shell, channel, or PTY history", async () => {
    await startContractFixture("incompatible");
    const marker = "ROOST_INCOMPATIBLE_LIVE";
    const channelId = 812;
    const spawned = await spawnThroughIncompatibleConnection(channelId, marker);
    const historyBefore = await readRawHistory(channelId);
    const before = await probeKeeperCompatible(ENDPOINT);
    expect(before.protocolCompatible).toBe(false);
    expect(before.bindings).toEqual([
      { channel_id: channelId, pid: spawned.shellPid },
    ]);

    await expect(handleKeeperSurvivor(new Set()))
      .rejects.toThrow(KEEPER_REPLACEMENT_BLOCKED_ERROR);
    await expect(handleKeeperSurvivor(new Set(["live-session"])))
      .rejects.toThrow(KEEPER_REPLACEMENT_BLOCKED_ERROR);

    const after = await probeKeeperCompatible(ENDPOINT);
    expect(after.keeperPid).toBe(before.keeperPid);
    expect(after.processEpoch).toBe(before.processEpoch);
    expect(after.bindings).toEqual(before.bindings);
    expect(isAlive(before.keeperPid!)).toBe(true);
    expect(isAlive(spawned.shellPid)).toBe(true);
    expect(existsSync(ENDPOINT.address)).toBe(true);
    expect(await readRawHistory(channelId)).toBe(historyBefore);
    expect(historyBefore).toContain(marker);
  }, 20_000);

  test("replaces an authenticated incompatible keeper only when both proofs are empty", async () => {
    await startContractFixture("incompatible");
    const before = await probeKeeperCompatible(ENDPOINT);
    expect(before).toMatchObject({
      reachable: true,
      authenticated: true,
      protocolCompatible: false,
      bindings: [],
      spawningChannels: [],
    });

    await handleKeeperSurvivor(new Set());
    expect((await probeKeeperCompatible(ENDPOINT, 250)).reachable).toBe(false);

    const replacementPool = new MultiplexedKeeperPool();
    pools.push(replacementPool);
    await replacementPool.ensure();
    const replacement = await probeKeeperCompatible(ENDPOINT);
    expect(replacement).toMatchObject({
      reachable: true,
      authenticated: true,
      protocolCompatible: true,
      exactTarget: true,
      bindings: [],
      spawningChannels: [],
    });
    expect(replacement.keeperPid).not.toBe(before.keeperPid);
    expect(replacement.processEpoch).not.toBe(before.processEpoch);
  }, 20_000);

  test("refuses a reachable pre-auth endpoint without shutdown or unlink", async () => {
    await prepareLocalEndpoint(ENDPOINT);
    const sockets = new Set<Socket>();
    const server = createServer((socket) => {
      sockets.add(socket);
      socket.on("close", () => sockets.delete(socket));
      socket.on("data", () => {});
    });
    const listening = Promise.withResolvers<void>();
    server.once("error", listening.reject);
    server.listen(ENDPOINT.address, () => listening.resolve());
    await listening.promise;
    try {
      await expect(handleKeeperSurvivor(new Set()))
        .rejects.toThrow(KEEPER_IDENTITY_UNPROVEN_ERROR);
      expect(server.listening).toBe(true);
      expect(existsSync(ENDPOINT.address)).toBe(true);
    } finally {
      await closePreAuthServer(server, sockets);
    }
  }, 20_000);
});
