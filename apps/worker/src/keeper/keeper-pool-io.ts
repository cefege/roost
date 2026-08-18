// Multiplexed keeper pool — per-channel IO (input/resize/kill), the shared
// null-socket-aware frame writer, and pool teardown. Free functions extracted
// from MultiplexedKeeperPool (multiplexed-client.ts); each takes the pool
// instance as its first argument. Behavior is unchanged.

import { signal } from "@roost/shared/diag";
import { cleanupLocalEndpoint } from "@roost/shared/local-endpoint";
import { log } from "@roost/shared/log";
import {
  KeeperFeature,
  KEEPER_MAX_INPUT_BYTES,
  MuxFrameType,
  encodeMuxFrame,
  encodePtyInRequestFrame,
  encodeResizeRequest,
  encodeResizeStatusQuery,
} from "./protocol.ts";
import { muxLocalEndpoint } from "./keeper-pool-config.ts";
import { monoNowMs } from "../util/mono.ts";
import type { MultiplexedKeeperPool } from "./multiplexed-client.ts";
import type {
  KeeperInputResult,
  KeeperResizeResult,
  KeeperTerminalState,
} from "./protocol.ts";

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

/** Reasons a keeper command provably never reached the socket. All four are
 *  shared by `PtyInFailureReason` and `ResizeRejectReason`, so an admission
 *  failure maps to a definite typed result without inventing a wire reason. */
export type KeeperWriteRejection =
  | "disconnected"
  | "unsupported"
  | "invalid_request"
  | "queue_full";

export type KeeperWriteAdmission =
  | { written: true }
  | { written: false; reason: KeeperWriteRejection };

/** Two-phase keeper command. `admission` is decided synchronously: the request
 *  frame either reached the current socket or provably never will, which is the
 *  only phase a caller may treat as definite. `result` settles later — a lost
 *  result after a successful write is ambiguous, never a rejection. */
export interface KeeperCommand<T> {
  admission: KeeperWriteAdmission;
  result: Promise<T>;
}

function unwritten<T>(reason: KeeperWriteRejection, result: T): KeeperCommand<T> {
  return { admission: { written: false, reason }, result: Promise.resolve(result) };
}

export function channelInputCommand(
  pool: MultiplexedKeeperPool,
  channelId: number,
  inputSeq: number,
  bytes: Uint8Array,
): KeeperCommand<KeeperInputResult> {
  const definite = (reason: KeeperWriteRejection): KeeperCommand<KeeperInputResult> =>
    unwritten(reason, { kind: "reject", inputSeq, writtenBytes: 0, reason });
  if (!pool.socket || pool.socket.destroyed) return definite("disconnected");
  if (!pool.supportsKeeperFeature(KeeperFeature.AcknowledgedInput)) return definite("unsupported");
  let frame: Buffer;
  try {
    frame = encodePtyInRequestFrame(channelId, { inputSeq, bytes });
  } catch {
    return definite("invalid_request");
  }
  const key = commandKey(channelId, inputSeq);
  const usage = pool._pendingInputUsage.get(channelId) ?? { commands: 0, bytes: 0 };
  if (pool.pendingInputs.has(key)
      || usage.commands >= MAX_PENDING_INPUT_COMMANDS
      || usage.bytes + bytes.byteLength > MAX_PENDING_INPUT_BYTES) {
    return definite("queue_full");
  }

  let admission: KeeperWriteAdmission = { written: true };
  const { promise, resolve } = Promise.withResolvers<KeeperInputResult>();
  pool.pendingInputs.set(key, {
    channelId,
    inputSeq,
    expectedBytes: bytes.byteLength,
    startedMonoMs: monoNowMs(),
    timer: setTimeout(() => {
      settlePendingInput(pool, channelId, inputSeq, {
        kind: "ambiguous",
        inputSeq,
        writtenBytes: null,
        reason: "timeout",
      });
    }, COMMAND_RESULT_TIMEOUT_MS),
    resolve,
  });
  usage.commands++;
  usage.bytes += bytes.byteLength;
  pool._pendingInputUsage.set(channelId, usage);
  try {
    pool.socket.write(frame);
  } catch {
    admission = { written: false, reason: "disconnected" };
    settlePendingInput(pool, channelId, inputSeq, {
      kind: "reject",
      inputSeq,
      writtenBytes: 0,
      reason: "disconnected",
    });
  }
  return { admission, result: promise };
}

export function settlePendingResize(
  pool: MultiplexedKeeperPool,
  channelId: number,
  seq: number,
  result: KeeperResizeResult,
  source: "frame" | "local" = "local",
): boolean {
  const key = commandKey(channelId, seq);
  const pending = pool.pendingResizes.get(key);
  if (!pending) return false;
  pool.pendingResizes.delete(key);
  clearTimeout(pending.timer);
  // The geometry boundary is a property of the keeper's ORDERED stream, not of
  // promise scheduling: a result frame and later PtyOut can share one socket
  // read, and awaiting the result promise costs at least one microtask. The
  // owner therefore marks its boundary here, synchronously, before any byte
  // that the keeper produced after the resize can be dispatched.
  if (source === "frame") pending.onResultFrame?.(result);
  pending.resolve(result);
  return true;
}

function resizeCommand(
  pool: MultiplexedKeeperPool,
  channelId: number,
  seq: number,
  frameType: MuxFrameType.ResizeRequest | MuxFrameType.ResizeStatus,
  payload: Buffer,
  onResultFrame?: (result: KeeperResizeResult) => void,
): KeeperCommand<KeeperResizeResult> {
  if (!pool.socket || pool.socket.destroyed) {
    return unwritten("disconnected", { kind: "reject", seq, reason: "disconnected" });
  }
  if (!pool.supportsKeeperFeature(KeeperFeature.AcknowledgedResize)) {
    return unwritten("unsupported", { kind: "reject", seq, reason: "unsupported" });
  }
  const key = commandKey(channelId, seq);
  if (pool.pendingResizes.has(key)) {
    // Another owner already wrote this exact sequence. This caller wrote
    // nothing, but the sequence's fate is not proven — stay unknown.
    return unwritten("queue_full", { kind: "unknown", seq, reason: "protocol_error" });
  }
  let admission: KeeperWriteAdmission = { written: true };
  const { promise, resolve } = Promise.withResolvers<KeeperResizeResult>();
  pool.pendingResizes.set(key, {
    channelId,
    seq,
    startedMonoMs: monoNowMs(),
    timer: setTimeout(() => {
      settlePendingResize(pool, channelId, seq, { kind: "unknown", seq, reason: "timeout" });
    }, COMMAND_RESULT_TIMEOUT_MS),
    resolve,
    onResultFrame,
  });
  try {
    pool.socket.write(encodeMuxFrame(frameType, channelId, payload));
  } catch {
    admission = { written: false, reason: "disconnected" };
    settlePendingResize(pool, channelId, seq, { kind: "reject", seq, reason: "disconnected" });
  }
  return { admission, result: promise };
}

/** Apply a logical resize sequence at most once. Admission proves the keeper
 *  received the request; only the result proves the PTY geometry. */
export function channelResizeCommand(
  pool: MultiplexedKeeperPool,
  channelId: number,
  seq: number,
  cols: number,
  rows: number,
  onResultFrame?: (result: KeeperResizeResult) => void,
): KeeperCommand<KeeperResizeResult> {
  let payload: Buffer;
  try {
    payload = encodeResizeRequest({ seq, cols, rows });
  } catch {
    return unwritten("invalid_request", { kind: "reject", seq, reason: "invalid_request" });
  }
  return resizeCommand(pool, channelId, seq, MuxFrameType.ResizeRequest, payload, onResultFrame);
}

/** Query the keeper's cached result without reapplying terminal.resize. This
 *  command never applies geometry, so writing it can never satisfy a resize
 *  admission on its own. */
export function channelResizeStatusCommand(
  pool: MultiplexedKeeperPool,
  channelId: number,
  seq: number,
  onResultFrame?: (result: KeeperResizeResult) => void,
): KeeperCommand<KeeperResizeResult> {
  let payload: Buffer;
  try {
    payload = encodeResizeStatusQuery({ seq });
  } catch {
    return unwritten("invalid_request", { kind: "reject", seq, reason: "invalid_request" });
  }
  return resizeCommand(pool, channelId, seq, MuxFrameType.ResizeStatus, payload, onResultFrame);
}

/** Authoritative resize sequence + current geometry straight from the keeper's
 *  live channel. `null` = unreachable or unsupported; the caller must then keep
 *  its own last proven size and leave its sequence floor invalid. */
export function channelTerminalState(
  pool: MultiplexedKeeperPool,
  channelId: number,
): Promise<KeeperTerminalState | null> {
  if (!pool.socket || pool.socket.destroyed) return Promise.resolve(null);
  if (!pool.supportsKeeperFeature(KeeperFeature.TerminalState)) return Promise.resolve(null);
  const { promise, resolve } = Promise.withResolvers<KeeperTerminalState | null>();
  const waiters = pool.pendingTerminalStates.get(channelId) ?? [];
  const waiter = {
    resolve,
    timer: setTimeout(() => {
      settlePendingTerminalState(pool, channelId, null);
    }, COMMAND_RESULT_TIMEOUT_MS),
  };
  waiters.push(waiter);
  pool.pendingTerminalStates.set(channelId, waiters);
  try {
    pool.socket.write(encodeMuxFrame(MuxFrameType.GetTerminalState, channelId, new Uint8Array(0)));
  } catch {
    settlePendingTerminalState(pool, channelId, null);
  }
  return promise;
}

/** Resolve the head waiter for a channel — the keeper answers in arrival
 *  order, so FIFO keeps request/response 1:1 across concurrent recoveries. */
export function settlePendingTerminalState(
  pool: MultiplexedKeeperPool,
  channelId: number,
  state: KeeperTerminalState | null,
): boolean {
  const waiters = pool.pendingTerminalStates.get(channelId);
  const waiter = waiters?.shift();
  if (waiters?.length === 0) pool.pendingTerminalStates.delete(channelId);
  if (!waiter) return false;
  clearTimeout(waiter.timer);
  waiter.resolve(state);
  return true;
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
  for (const channelId of [...pool.pendingTerminalStates.keys()]) {
    while (settlePendingTerminalState(pool, channelId, null)) { /* drain FIFO */ }
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
