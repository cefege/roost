// A fake keeper SOCKET, not a fake keeper client.
//
// The real two-phase command path (keeper-pool-io.ts) runs unchanged: pending
// maps, per-command watchdogs, admission, and the synchronous result-frame hook.
// Only the socket is replaced, so the frames the worker would put on the wire are
// captured verbatim and results are injected exactly where the real inbound
// dispatcher injects them — including `source: "frame"`, which is what makes the
// geometry-boundary hook run synchronously before later PtyOut.

import {
  MuxFrameType,
  SUPPORTED_KEEPER_FEATURES,
  decodeMuxFrames,
  decodePtyInRequest,
  decodeResizeRequest,
  decodeResizeStatusQuery,
} from "../src/keeper/protocol.ts";
import type { KeeperResizeResult, KeeperTerminalState } from "../src/keeper/protocol.ts";
import {
  settlePendingInput,
  settlePendingResize,
  settlePendingTerminalState,
} from "../src/keeper/keeper-pool-io.ts";
import { getMultiplexedPool } from "../src/keeper/multiplexed-client.ts";
import type { MultiplexedKeeperPool } from "../src/keeper/multiplexed-client.ts";


/** `MuxFrameType` is a const enum, so it has no runtime reverse map. Only the
 *  frames this fixture asserts order on need a readable name. */
const FRAME_NAMES: Readonly<Record<number, string>> = {
  [MuxFrameType.ResizeRequest]: "ResizeRequest",
  [MuxFrameType.ResizeStatus]: "ResizeStatus",
  [MuxFrameType.PtyInRequest]: "PtyInRequest",
  [MuxFrameType.PtyIn]: "PtyIn",
  [MuxFrameType.GetTerminalState]: "GetTerminalState",
  [MuxFrameType.GetHistoryRecords]: "GetHistoryRecords",
  [MuxFrameType.KillChild]: "KillChild",
};
export interface KeeperWrite {
  type: MuxFrameType;
  channelId: number;
  /** Decoded sequence for resize/status/input frames. */
  seq: number | null;
  cols: number | null;
  rows: number | null;
  bytes: Uint8Array | null;
}

export interface FakeKeeper {
  writes: KeeperWrite[];
  /** Frame-kind shorthand for order assertions. */
  order(): string[];
  seqOf(type: MuxFrameType): number[];
  /** Resolve on the first write of this kind, past or future. The keeper socket
   *  is the only truthful "the worker got here" signal; polling microtask counts
   *  instead makes a test's pass depend on how many hops an outcome happens to
   *  take. */
  waitForWrite(type: MuxFrameType): Promise<KeeperWrite>;
  resizeAck(channelId: number, seq: number, cols: number, rows: number): void;
  resizeReject(channelId: number, seq: number, reason: "unknown_sequence" | "channel_exited" | "resize_error"): void;
  /** A result the worker cannot interpret: proves nothing about the PTY. */
  resizeUnknown(channelId: number, seq: number): void;
  inputAck(channelId: number, seq: number, writtenBytes: number): void;
  terminalState(channelId: number, state: KeeperTerminalState | null): void;
  restore(): void;
}

export interface FakeKeeperOptions {
  /** Omit a name to model a deployed keeper that predates it. Feature
   *  negotiation is per socket, so this is the production fallback path. */
  features?: readonly string[];
  /** Runs one microtask after the frame is written — a keeper cannot answer
   *  inside the caller's write. */
  onWrite?: (write: KeeperWrite) => void;
}

/** Replace the pool socket + negotiated features. */
export function installFakeKeeper(opts: FakeKeeperOptions = {}): FakeKeeper {
  const features = opts.features ?? SUPPORTED_KEEPER_FEATURES;
  const pool: MultiplexedKeeperPool = getMultiplexedPool();
  const priorSocket = pool.socket;
  const priorFeatures = [...pool.keeperFeatures];
  const priorConnect = pool.connectPromise;
  // A resolved connect promise makes ensureKeeper() a no-op, so nothing in the
  // fixture can spawn or adopt a REAL keeper whose late connect would overwrite
  // pool.socket mid-test and turn every later write into "disconnected".
  pool.connectPromise = Promise.resolve();
  const writes: KeeperWrite[] = [];
  let carry: Buffer = Buffer.alloc(0);
  const writeWaiters: Array<{ type: MuxFrameType; resolve: (write: KeeperWrite) => void }> = [];

  pool.socket = {
    destroyed: false,
    write: (frame: Buffer): boolean => {
      carry = Buffer.concat([carry, frame]);
      const { frames, remaining } = decodeMuxFrames(carry);
      carry = remaining;
      for (const f of frames) {
        const write: KeeperWrite = {
          type: f.type,
          channelId: f.channelId,
          seq: null,
          cols: null,
          rows: null,
          bytes: null,
        };
        if (f.type === MuxFrameType.ResizeRequest) {
          const request = decodeResizeRequest(f.payload);
          if (request) {
            write.seq = request.seq;
            write.cols = request.cols;
            write.rows = request.rows;
          }
        } else if (f.type === MuxFrameType.ResizeStatus) {
          write.seq = decodeResizeStatusQuery(f.payload)?.seq ?? null;
        } else if (f.type === MuxFrameType.PtyInRequest) {
          const request = decodePtyInRequest(f.payload);
          if (request) {
            write.seq = request.inputSeq;
            write.bytes = Uint8Array.from(request.bytes);
          }
        } else if (f.type === MuxFrameType.PtyIn) {
          write.bytes = Uint8Array.from(f.payload);
        }
        writes.push(write);
        for (let i = writeWaiters.length - 1; i >= 0; i--) {
          if (writeWaiters[i]!.type !== write.type) continue;
          writeWaiters.splice(i, 1)[0]!.resolve(write);
        }
        if (opts.onWrite) queueMicrotask(() => opts.onWrite!(write));
      }
      return true;
    },
  } as never;
  pool.setKeeperFeatures(features);

  const settleResize = (channelId: number, result: KeeperResizeResult): void => {
    settlePendingResize(pool, channelId, result.seq, result, "frame");
  };
  return {
    writes,
    order: () => writes.map((w) => FRAME_NAMES[w.type] ?? String(w.type)),
    seqOf: (type) => writes.filter((w) => w.type === type).map((w) => w.seq ?? -1),
    waitForWrite: (type) => {
      const seen = writes.find((w) => w.type === type);
      if (seen) return Promise.resolve(seen);
      const { promise, resolve } = Promise.withResolvers<KeeperWrite>();
      writeWaiters.push({ type, resolve });
      return promise;
    },
    resizeAck: (channelId, seq, cols, rows) =>
      settleResize(channelId, { kind: "ack", seq, cols, rows }),
    resizeReject: (channelId, seq, reason) =>
      settleResize(channelId, { kind: "reject", seq, reason }),
    resizeUnknown: (channelId, seq) =>
      settleResize(channelId, { kind: "unknown", seq, reason: "protocol_error" }),
    inputAck: (channelId, seq, writtenBytes) => {
      settlePendingInput(pool, channelId, seq, { kind: "ack", inputSeq: seq, writtenBytes });
    },
    terminalState: (channelId, state) => {
      settlePendingTerminalState(pool, channelId, state);
    },
    restore: () => {
      pool.socket = priorSocket;
      pool.setKeeperFeatures(priorFeatures);
      pool.connectPromise = priorConnect;
      for (const key of [...pool.pendingResizes.keys()]) {
        const pending = pool.pendingResizes.get(key)!;
        settlePendingResize(pool, pending.channelId, pending.seq, {
          kind: "unknown",
          seq: pending.seq,
          reason: "disconnected",
        });
      }
      for (const key of [...pool.pendingInputs.keys()]) {
        const pending = pool.pendingInputs.get(key)!;
        settlePendingInput(pool, pending.channelId, pending.inputSeq, {
          kind: "ambiguous",
          inputSeq: pending.inputSeq,
          writtenBytes: null,
          reason: "disconnected",
        });
      }
      for (const channelId of [...pool.pendingTerminalStates.keys()]) {
        while (settlePendingTerminalState(pool, channelId, null)) { /* drain */ }
      }
    },
  };
}

interface AutoChannel {
  cols: number;
  rows: number;
  highestSeq: number;
  appliedSeq: number;
}

/** A keeper that answers. Same fake socket, plus the keeper's own idempotence
 *  rules: a sequence at or below the highest consumed one is `unknown_sequence`,
 *  a status query answers from the cache, and terminal state reports live channel
 *  values. Fixtures that assert the SCD size the worker PROVED need this — with
 *  no keeper, every resize is a truthful pre-write rejection and nothing is ever
 *  proven. Replies land on a microtask: a keeper cannot answer inside the write. */
export function installAutoKeeper(initial: { cols: number; rows: number }): FakeKeeper {
  const channels = new Map<number, AutoChannel>();
  const channelOf = (channelId: number): AutoChannel => {
    let channel = channels.get(channelId);
    if (!channel) {
      channel = { cols: initial.cols, rows: initial.rows, highestSeq: 0, appliedSeq: 0 };
      channels.set(channelId, channel);
    }
    return channel;
  };
  const keeper = installFakeKeeper({
    onWrite: (write) => {
      const channel = channelOf(write.channelId);
      const seq = write.seq ?? 0;
      switch (write.type) {
        case MuxFrameType.ResizeRequest:
          if (seq <= channel.highestSeq) {
            keeper.resizeReject(write.channelId, seq, "unknown_sequence");
            return;
          }
          channel.highestSeq = seq;
          channel.appliedSeq = seq;
          channel.cols = write.cols ?? channel.cols;
          channel.rows = write.rows ?? channel.rows;
          keeper.resizeAck(write.channelId, seq, channel.cols, channel.rows);
          return;
        case MuxFrameType.ResizeStatus:
          if (seq > 0 && seq === channel.appliedSeq) {
            keeper.resizeAck(write.channelId, seq, channel.cols, channel.rows);
          } else {
            keeper.resizeReject(write.channelId, seq, "unknown_sequence");
          }
          return;
        case MuxFrameType.GetTerminalState:
          keeper.terminalState(write.channelId, {
            headSeq: 0,
            cols: channel.cols,
            rows: channel.rows,
            highestResizeSeq: channel.highestSeq,
            appliedResizeSeq: channel.appliedSeq,
          });
          return;
        case MuxFrameType.PtyInRequest:
          keeper.inputAck(write.channelId, seq, write.bytes?.byteLength ?? 0);
          return;
        default:
          return;
      }
    },
  });
  return keeper;
}
