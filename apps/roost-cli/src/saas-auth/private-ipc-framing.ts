/**
 * Reads and writes one bounded length-prefixed frame on the private provisioner socket.
 * Both IPC client and server depend on this framing before parsing signed JSON envelopes.
 * Timeouts and exact-frame checks prevent truncation, multiplexing, and unbounded buffering.
 */

import { Buffer } from "node:buffer";
import { createConnection, type Socket } from "node:net";
import { PRIVATE_IPC_MAX_FRAME_BYTES, PRIVATE_IPC_TIMEOUT_MS } from "./private-ipc.ts";

export class PrivateIpcFrameError extends Error {
  constructor(readonly code: "oversized" | "truncated" | "multiple" | "timeout" | "io", message: string) {
    super(message);
    this.name = "PrivateIpcFrameError";
  }
}

export function encodePrivateIpcFrame(payload: Uint8Array, maxBytes = PRIVATE_IPC_MAX_FRAME_BYTES): Buffer {
  if (!Number.isSafeInteger(maxBytes) || maxBytes <= 0 || payload.byteLength === 0 || payload.byteLength > maxBytes) throw new PrivateIpcFrameError("oversized", "private IPC payload size is invalid");
  const frame = Buffer.allocUnsafe(4 + payload.byteLength);
  frame.writeUInt32BE(payload.byteLength, 0);
  Buffer.from(payload).copy(frame, 4);
  return frame;
}

export function readPrivateIpcFrame(socket: Socket, options: { maxBytes?: number; timeoutMs?: number } = {}): Promise<Buffer> {
  const maxBytes = options.maxBytes ?? PRIVATE_IPC_MAX_FRAME_BYTES;
  const timeoutMs = options.timeoutMs ?? PRIVATE_IPC_TIMEOUT_MS;
  const { promise, resolve, reject } = Promise.withResolvers<Buffer>();
  let buffered = Buffer.alloc(0);
  let expected: number | null = null;
  let settled = false;
  const totalTimer = setTimeout(() => fail(new PrivateIpcFrameError("timeout", "private IPC total deadline exceeded")), timeoutMs);
  let idleTimer = setTimeout(() => fail(new PrivateIpcFrameError("timeout", "private IPC idle deadline exceeded")), timeoutMs);
  const cleanup = () => { clearTimeout(totalTimer); clearTimeout(idleTimer); socket.off("data", onData); socket.off("end", onEnd); socket.off("error", onError); };
  const fail = (error: Error) => { if (settled) return; settled = true; cleanup(); reject(error); };
  const finish = (payload: Buffer) => { if (settled) return; settled = true; cleanup(); resolve(payload); };
  const resetIdle = () => { clearTimeout(idleTimer); idleTimer = setTimeout(() => fail(new PrivateIpcFrameError("timeout", "private IPC idle deadline exceeded")), timeoutMs); };
  const onData = (chunk: Buffer) => {
    resetIdle();
    if (buffered.length + chunk.length > maxBytes + 4) return fail(new PrivateIpcFrameError("multiple", "private IPC connection contains extra frame data"));
    buffered = Buffer.concat([buffered, chunk]);
    if (expected === null && buffered.length >= 4) {
      expected = buffered.readUInt32BE(0);
      if (expected === 0 || expected > maxBytes) return fail(new PrivateIpcFrameError("oversized", "private IPC frame length is invalid"));
    }
    if (expected !== null && buffered.length > expected + 4) fail(new PrivateIpcFrameError("multiple", "private IPC connection contains multiple frames"));
  };
  const onEnd = () => {
    if (expected !== null && buffered.length === expected + 4) finish(buffered.subarray(4));
    else fail(new PrivateIpcFrameError("truncated", "private IPC frame ended before its declared length"));
  };
  const onError = () => fail(new PrivateIpcFrameError("io", "private IPC socket failed"));
  socket.on("data", onData); socket.once("end", onEnd); socket.once("error", onError);
  return promise;
}

export async function exchangePrivateIpcFrame(socketPath: string, payload: Uint8Array, options: { maxBytes?: number; timeoutMs?: number } = {}): Promise<Buffer> {
  if (!socketPath.startsWith("/")) throw new PrivateIpcFrameError("io", "private IPC socket path must be absolute");
  const timeoutMs = options.timeoutMs ?? PRIVATE_IPC_TIMEOUT_MS;
  const socket = createConnection({ path: socketPath });
  try {
    const { promise, resolve, reject } = Promise.withResolvers<void>();
    const timer = setTimeout(() => { cleanup(); reject(new PrivateIpcFrameError("timeout", "private IPC connect deadline exceeded")); }, timeoutMs);
    const cleanup = () => { clearTimeout(timer); socket.off("connect", connected); socket.off("error", failed); };
    const connected = () => { cleanup(); resolve(); };
    const failed = () => { cleanup(); reject(new PrivateIpcFrameError("io", "private IPC connection failed")); };
    socket.once("connect", connected); socket.once("error", failed);
    await promise;
    const response = readPrivateIpcFrame(socket, options);
    socket.end(encodePrivateIpcFrame(payload, options.maxBytes));
    return await response;
  } finally {
    socket.destroy();
  }
}
