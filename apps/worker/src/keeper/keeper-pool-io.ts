// Multiplexed keeper pool — per-channel IO (input/resize/kill), the shared
// null-socket-aware frame writer, and pool teardown. Free functions extracted
// from MultiplexedKeeperPool (multiplexed-client.ts); each takes the pool
// instance as its first argument. Behavior is unchanged.

import { cleanupLocalEndpoint, log, signal } from "@roost/shared";
import {
  KeeperFeature,
  KEEPER_MAX_INPUT_BYTES,
  MuxFrameType,
  encodeMuxFrame,
  encodePtyInRequestFrame,
  encodeResizeRequest,
  encodeResizeStatusQuery,
} from "./protocol-v2.ts";
import { muxLocalEndpoint } from "./keeper-pool-config.ts";
import type { MultiplexedKeeperPool } from "./multiplexed-client.ts";
import type {
  KeeperInputResult,
  KeeperResizeResult,
} from "./protocol-v2.ts";

const DROP_LOG_INTERVAL_MS = 1000;
const COMMAND_RESULT_TIMEOUT_MS = 2500;
const MAX_PENDING_INPUT_COMMANDS = 200;
const MAX_PENDING_INPUT_BYTES = 256 * 1024;

function commandKey(channelId: number, seq: number): string {
  return `${channelId}:${seq}`;
}

/** Write a frame to the keeper socket with consistent null-socket
 *  handling. `logTarget` null = silent drop (kill path); otherwise
 *  log.warn so the gap is visible (rate-limited to one line per
 *  second per target). The close handler (line ~122) already fired
 *  onExit(null) + channels.clear() for any channel the worker thinks
 *  is alive, so replaying frames after reconnect is unsafe — the
 *  new keeper PID doesn't know the prior channelId. */
function writeFrame(
  pool: MultiplexedKeeperPool,
  frame: Buffer,
  logTarget: string | null,
  dropFields: Record<string, unknown>,
): boolean {
  if (!pool.socket || pool.socket.destroyed) {
    if (logTarget !== null) {
      const now = Date.now();
      const state = pool._dropLogState.get(logTarget) ?? { lastTs: 0, suppressed: 0 };
      if (now - state.lastTs >= DROP_LOG_INTERVAL_MS) {
        log.warn(logTarget, "dropped: socket closed", { ...dropFields, suppressedSince: state.suppressed });
        pool._dropLogState.set(logTarget, { lastTs: now, suppressed: 0 });
      } else {
        pool._dropLogState.set(logTarget, { lastTs: state.lastTs, suppressed: state.suppressed + 1 });
      }
      if (logTarget === "mux-pool.input") {
        signal("input.drop_burst", {
          sid: dropFields.sid,
          reason: "keeper_socket_closed",
          cooldownKey: dropFields.sid,
        });
      }
    }
    return false;
  }
  pool.socket.write(frame);
  return true;
}

export function channelInput(pool: MultiplexedKeeperPool, channelId: number, bytes: Uint8Array): void {
  writeFrame(
    pool,
    encodeMuxFrame(MuxFrameType.PtyIn, channelId, bytes),
    "mux-pool.input",
    { channelId, len: bytes.length },
  );
}

export function settlePendingInput(
  pool: MultiplexedKeeperPool,
  channelId: number,
  inputSeq: number,
  received: KeeperInputResult,
): boolean {
  const key = commandKey(channelId, inputSeq);
  const pending = pool.pendingInputs.get(key);
  if (!pending) return false;
  pool.pendingInputs.delete(key);
  clearTimeout(pending.timer);
  const usage = pool._pendingInputUsage.get(channelId);
  if (usage) {
    usage.commands--;
    usage.bytes -= pending.expectedBytes;
    if (usage.commands === 0) pool._pendingInputUsage.delete(channelId);
  }
  const result = received.writtenBytes !== null
      && (received.writtenBytes > pending.expectedBytes
        || (received.kind === "ack" && received.writtenBytes !== pending.expectedBytes))
    ? {
        kind: "ambiguous" as const,
        inputSeq,
        writtenBytes: null,
        reason: "protocol_error" as const,
      }
    : received;
  pending.resolve(result);
  return true;
}

export function channelInputRequest(
  pool: MultiplexedKeeperPool,
  channelId: number,
  inputSeq: number,
  bytes: Uint8Array,
): Promise<KeeperInputResult> {
  if (!pool.socket || pool.socket.destroyed) {
    return Promise.resolve({ kind: "reject", inputSeq, writtenBytes: 0, reason: "disconnected" });
  }
  if (!pool.supportsKeeperFeature(KeeperFeature.AcknowledgedInput)) {
    return Promise.resolve({ kind: "reject", inputSeq, writtenBytes: 0, reason: "unsupported" });
  }
  let frame: Buffer;
  try {
    frame = encodePtyInRequestFrame(channelId, { inputSeq, bytes });
  } catch {
    return Promise.resolve({ kind: "reject", inputSeq, writtenBytes: 0, reason: "invalid_request" });
  }
  const key = commandKey(channelId, inputSeq);
  const usage = pool._pendingInputUsage.get(channelId) ?? { commands: 0, bytes: 0 };
  if (pool.pendingInputs.has(key)
      || usage.commands >= MAX_PENDING_INPUT_COMMANDS
      || usage.bytes + bytes.byteLength > MAX_PENDING_INPUT_BYTES) {
    return Promise.resolve({ kind: "reject", inputSeq, writtenBytes: 0, reason: "queue_full" });
  }

  return new Promise<KeeperInputResult>((resolve) => {
    const pending = {
      channelId,
      inputSeq,
      expectedBytes: bytes.byteLength,
      timer: setTimeout(() => {
        settlePendingInput(pool, channelId, inputSeq, {
          kind: "ambiguous",
          inputSeq,
          writtenBytes: null,
          reason: "timeout",
        });
      }, COMMAND_RESULT_TIMEOUT_MS),
      resolve,
    };
    pool.pendingInputs.set(key, pending);
    usage.commands++;
    usage.bytes += bytes.byteLength;
    pool._pendingInputUsage.set(channelId, usage);
    try {
      pool.socket!.write(frame);
    } catch {
      settlePendingInput(pool, channelId, inputSeq, {
        kind: "reject",
        inputSeq,
        writtenBytes: 0,
        reason: "disconnected",
      });
    }
  });
}

export function channelResize(pool: MultiplexedKeeperPool, channelId: number, cols: number, rows: number): void {
  writeFrame(
    pool,
    encodeMuxFrame(MuxFrameType.Resize, channelId, JSON.stringify({ cols, rows })),
    "mux-pool.resize",
    { channelId, cols, rows },
  );
}

export function settlePendingResize(
  pool: MultiplexedKeeperPool,
  channelId: number,
  seq: number,
  result: KeeperResizeResult,
): boolean {
  const key = commandKey(channelId, seq);
  const pending = pool.pendingResizes.get(key);
  if (!pending) return false;
  pool.pendingResizes.delete(key);
  clearTimeout(pending.timer);
  pending.resolve(result);
  return true;
}

function sendResizeCommand(
  pool: MultiplexedKeeperPool,
  channelId: number,
  seq: number,
  frameType: MuxFrameType.ResizeRequest | MuxFrameType.ResizeStatus,
  payload: Buffer,
): Promise<KeeperResizeResult> {
  if (!pool.socket || pool.socket.destroyed) {
    return Promise.resolve({ kind: "reject", seq, reason: "disconnected" });
  }
  if (!pool.supportsKeeperFeature(KeeperFeature.AcknowledgedResize)) {
    return Promise.resolve({ kind: "reject", seq, reason: "unsupported" });
  }
  const key = commandKey(channelId, seq);
  if (pool.pendingResizes.has(key)) {
    return Promise.resolve({ kind: "unknown", seq, reason: "protocol_error" });
  }
  return new Promise<KeeperResizeResult>((resolve) => {
    const pending = {
      channelId,
      seq,
      timer: setTimeout(() => {
        settlePendingResize(pool, channelId, seq, { kind: "unknown", seq, reason: "timeout" });
      }, COMMAND_RESULT_TIMEOUT_MS),
      resolve,
    };
    pool.pendingResizes.set(key, pending);
    try {
      pool.socket!.write(encodeMuxFrame(frameType, channelId, payload));
    } catch {
      settlePendingResize(pool, channelId, seq, { kind: "reject", seq, reason: "disconnected" });
    }
  });
}

export function channelResizeRequest(
  pool: MultiplexedKeeperPool,
  channelId: number,
  seq: number,
  cols: number,
  rows: number,
): Promise<KeeperResizeResult> {
  try {
    return sendResizeCommand(
      pool,
      channelId,
      seq,
      MuxFrameType.ResizeRequest,
      encodeResizeRequest({ seq, cols, rows }),
    );
  } catch {
    return Promise.resolve({ kind: "reject", seq, reason: "invalid_request" });
  }
}

export function channelResizeStatus(
  pool: MultiplexedKeeperPool,
  channelId: number,
  seq: number,
): Promise<KeeperResizeResult> {
  try {
    return sendResizeCommand(
      pool,
      channelId,
      seq,
      MuxFrameType.ResizeStatus,
      encodeResizeStatusQuery({ seq }),
    );
  } catch {
    return Promise.resolve({ kind: "reject", seq, reason: "invalid_request" });
  }
}

export function settlePendingKeeperCommandsOnDisconnect(pool: MultiplexedKeeperPool): void {
  for (const pending of [...pool.pendingInputs.values()]) {
    settlePendingInput(pool, pending.channelId, pending.inputSeq, {
      kind: "ambiguous",
      inputSeq: pending.inputSeq,
      writtenBytes: null,
      reason: "disconnected",
    });
  }
  for (const pending of [...pool.pendingResizes.values()]) {
    settlePendingResize(pool, pending.channelId, pending.seq, {
      kind: "unknown",
      seq: pending.seq,
      reason: "disconnected",
    });
  }
  pool.keeperFeatures.clear();
}

export function channelKill(pool: MultiplexedKeeperPool, channelId: number): void {
  // Silent on null socket (expected — close handler already cascaded
  // through SessionManager.closedByKeeper).
  writeFrame(
    pool,
    encodeMuxFrame(MuxFrameType.KillChild, channelId, new Uint8Array(0)),
    null,
    {},
  );
}

export function disposePool(pool: MultiplexedKeeperPool): void {
  if (pool.socket) {
    try { pool.socket.destroy(); } catch { /* ignore */ }
    pool.socket = null;
  }
  void cleanupLocalEndpoint(muxLocalEndpoint());
}
