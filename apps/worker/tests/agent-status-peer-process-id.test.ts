// Kernel peer-process reader tests cover Bun's accepted-socket handle seam.
// Focused injections pin fail-closed validation; a real child connection
// proves this platform's native library, constants, and handle shape together.
import { expect, test } from "bun:test";
import { randomUUID } from "node:crypto";
import net from "node:net";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Socket } from "node:net";
import { createLocalPeerProcessIdReader } from "../src/agent-status/peer-process-id.ts";

function acceptedSocket(handle: Record<string, unknown> | undefined): Socket {
  return { _handle: handle } as unknown as Socket;
}

test("passes the accepted Bun socket descriptor to the native peer query", () => {
  const queried: number[] = [];
  let closed = false;
  const reader = createLocalPeerProcessIdReader({
    platform: "linux",
    nativeQuery: {
      read: (nativeHandle) => {
        queried.push(nativeHandle);
        return 4_321;
      },
      close: () => { closed = true; },
    },
  });

  expect(reader.available).toBe(true);
  expect(reader.read(acceptedSocket({ fd: 27 }))).toBe(4_321);
  expect(queried).toEqual([27]);
  reader.close();
  expect(closed).toBe(true);
});

test("accepts the named-pipe handle shape and rejects missing handles", () => {
  let calls = 0;
  const reader = createLocalPeerProcessIdReader({
    platform: "win32",
    nativeQuery: {
      read: () => { calls++; return 99; },
      close: () => undefined,
    },
  });

  expect(reader.read(acceptedSocket({ handle: 45n }))).toBe(99);
  expect(reader.read(acceptedSocket(undefined))).toBeNull();
  expect(reader.read(acceptedSocket({ fd: Number.MAX_SAFE_INTEGER + 1 }))).toBeNull();
  expect(calls).toBe(1);
  reader.close();
});

test("fails closed on invalid native results and query failures", () => {
  let result: number | null = 0;
  const reader = createLocalPeerProcessIdReader({
    platform: "darwin",
    nativeQuery: {
      read: () => {
        if (result === null) throw new Error("native query failed");
        return result;
      },
      close: () => undefined,
    },
  });
  const socket = acceptedSocket({ fd: 31 });

  expect(reader.read(socket)).toBeNull();
  result = -1;
  expect(reader.read(socket)).toBeNull();
  result = 4_567;
  expect(reader.read(socket)).toBe(4_567);
  result = null;
  expect(reader.read(socket)).toBeNull();
  reader.close();
});


test("reads the actual PID of a separate local-socket client", async () => {
  const directory = await mkdtemp(join(tmpdir(), "roost-peer-pid-"));
  const endpoint = process.platform === "win32"
    ? `\\\\.\\pipe\\roost-peer-pid-${randomUUID()}`
    : join(directory, "peer.sock");
  const reader = createLocalPeerProcessIdReader();
  if (!reader.available && process.platform === "win32") {
    reader.close();
    await rm(directory, { recursive: true, force: true });
    return;
  }
  expect(reader.available).toBe(true);
  const observed = Promise.withResolvers<number | null>();
  const server = net.createServer((socket) => {
    observed.resolve(reader.read(socket));
  });
  const listening = Promise.withResolvers<void>();
  server.listen(endpoint, listening.resolve);
  await listening.promise;
  const child = Bun.spawn([
    process.execPath,
    "-e",
    [
      'const net = require("node:net");',
      'const socket = net.createConnection(process.env.ROOST_TEST_ENDPOINT);',
      'socket.on("connect", () => socket.end());',
      'socket.on("error", () => process.exit(2));',
    ].join(""),
  ], {
    env: { ...process.env, ROOST_TEST_ENDPOINT: endpoint },
    stdout: "ignore",
    stderr: "ignore",
  });

  try {
    const [peerPid, exitCode] = await Promise.all([observed.promise, child.exited]);
    expect(exitCode).toBe(0);
    expect(peerPid).toBe(child.pid);
  } finally {
    const closed = Promise.withResolvers<void>();
    server.close(() => closed.resolve());
    await closed.promise;
    reader.close();
    await rm(directory, { recursive: true, force: true });
  }
});