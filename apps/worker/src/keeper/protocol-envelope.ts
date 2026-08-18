// Frame envelope + spawn frames for the multiplexed keeper protocol. The wire
// format diagram and the wire-version history live in ./protocol.ts, which
// re-exports this module and its two siblings (protocol-io, protocol-terminal).
import { isShellSpec } from "../shell-spec.ts";
import type { ShellSpec } from "../shell-spec.ts";

/** Frame type tags. */
export const enum MuxFrameType {
  Spawn      = 0x10, // client → keeper: JSON {channel_id, cols, rows, shell_spec} — open a new PTY
  SpawnAck   = 0x11, // keeper → client: JSON {channel_id, pid} — PTY allocated
  SpawnErr   = 0x12, // keeper → client: JSON {channel_id, error}

  PtyIn            = 0x20, // legacy client → keeper: raw input bytes
  PtyOut           = 0x21, // keeper → client: raw output bytes
  PtyInRequest     = 0x22, // client → keeper: [input_seq:u64][bytes]
  PtyInAck         = 0x23, // keeper → client: [input_seq:u64][written:u32]
  PtyInReject      = 0x24, // keeper → client: [input_seq:u64][written=0:u32][reason:u8]
  PtyInAmbiguous   = 0x25, // keeper → client: [input_seq:u64][written:u32][reason:u8]

  Resize           = 0x30, // legacy client → keeper: JSON {cols, rows}
  KillChild        = 0x31, // client → keeper: no payload — terminate the PTY child
  Exit             = 0x32, // keeper → client: JSON {exit_code: number | null}
  ResizeRequest    = 0x33, // client → keeper: [seq:u64][cols:u32][rows:u32]
  ResizeAck        = 0x34, // keeper → client: [seq:u64][cols:u32][rows:u32]
  ResizeReject     = 0x35, // keeper → client: [seq:u64][reason:u8]
  ResizeStatus     = 0x36, // client → keeper: [seq:u64] cached-status query

  Ping       = 0xF0, // both, global (channel=0): liveness probe
  Pong       = 0xF1,

  // Cross-process resume — added so a fresh worker can discover the
  // channels surviving in a long-lived multiplexed-keeper process.
  ListChannels     = 0xE0, // client → keeper: no payload (channel=0)
  ListChannelsResp = 0xE1, // keeper → client: JSON {channels:[{channel_id,pid}]}

  // Version handshake — fresh worker probes a survivor keeper for
  // protocol compatibility before trusting it for resume. Keepers from
  // a prior commit either pre-date this frame (silent drop → handshake
  // timeout → treat as incompatible) or reply with a stale version
  // number. See KEEPER_PROTOCOL_VERSION below + apps/worker/src/main.ts
  // killStaleKeeper gate.
  Hello            = 0xE2, // client → keeper: JSON {version:number} (channel=0)
  HelloResp        = 0xE3, // keeper → client: JSON {version:number, build?:string}
  // `build` = KEEPER_BUILD_STAMP (keeper-stamp.ts), ADDITIVE + NON-gating: it
  // surfaces stale keeper CODE (probeKeeperCompatible reports it) but does NOT
  // enter the kill gate, so a pre-stamp keeper (no `build`) is not killed —
  // only flagged. Adding it needs NO version bump (absence is detectable).

  // Cross-process history resume. GetHistory remains only for draining a
  // deployed keeper that predates ordered resize records.
  GetHistory        = 0xE4, // legacy request: no payload (per channel)
  GetHistoryResp    = 0xE5, // legacy response: [head_seq:u64][ring bytes]
  GetHistoryRecords = 0xE6, // request: no payload (per channel)
  GetHistoryRecordsResp = 0xE7, // ordered Output/Resize record payload

  // Authoritative geometry recovery. A lost ResizeAck leaves the worker unable
  // to prove which sequence the keeper consumed; retained history markers can
  // be evicted, so the keeper answers from its live channel state instead.
  GetTerminalState     = 0xEA, // client → keeper: no payload (per channel)
  GetTerminalStateResp = 0xEB, // keeper → client: authoritative resize state

  // Authenticated administrative shutdown. The endpoint layer authenticates
  // Hello before either frame can be dispatched.
  Shutdown          = 0xE8, // client → keeper: empty payload (channel=0)
  ShutdownAck       = 0xE9, // keeper → client: empty payload (channel=0)
}

export interface MuxFrame {
  type: MuxFrameType;
  channelId: number;
  payload: Buffer;
}

export const KEEPER_MAX_MUX_FRAME_BYTES = 16 * 1024 * 1024;

// The scalar codecs and frame allocator below are shared with protocol-io.ts and
// protocol-terminal.ts; they are protocol internals, not part of the keeper's
// wire surface.
export function allocateMuxFrame(type: MuxFrameType, channelId: number, payloadLength: number): Buffer {
  if (!Number.isInteger(channelId) || channelId < 0 || channelId > 0xffff
      || !Number.isSafeInteger(payloadLength) || payloadLength < 0
      || payloadLength > KEEPER_MAX_MUX_FRAME_BYTES - 3) {
    throw new RangeError("invalid multiplexed keeper frame");
  }
  const total = 3 + payloadLength;
  const out = Buffer.allocUnsafe(4 + total);
  out.writeUInt32BE(total, 0);
  out[4] = type;
  out.writeUInt16BE(channelId, 5);
  return out;
}

/** Encode a single multiplexed frame. */
export function encodeMuxFrame(
  type: MuxFrameType, channelId: number, payload: Uint8Array | string,
): Buffer {
  const payloadLen = typeof payload === "string"
    ? Buffer.byteLength(payload, "utf8")
    : payload.byteLength;
  const out = allocateMuxFrame(type, channelId, payloadLen);
  // One final-frame allocation and one defensive copy. In particular, do not
  // construct an intermediary Buffer for pooled Bun PTY views.
  if (typeof payload === "string") out.write(payload, 7, payloadLen, "utf8");
  else out.set(payload, 7);
  return out;
}

/** Decode multiplexed frames from a streaming buffer. */
export function decodeMuxFrames(buf: Buffer): {
  frames: MuxFrame[];
  remaining: Buffer;
} {
  const frames: MuxFrame[] = [];
  let offset = 0;
  while (offset + 4 <= buf.length) {
    const total = buf.readUInt32BE(offset);
    if (total < 3 || total > KEEPER_MAX_MUX_FRAME_BYTES) {
      throw new RangeError(`invalid multiplexed keeper frame length: ${total}`);
    }
    if (offset + 4 + total > buf.length) break;
    const type = buf[offset + 4] as MuxFrameType;
    const channelId = buf.readUInt16BE(offset + 5);
    // Consumers in multiplexed-client.ts (onOutput/spawnAck/etc.) read
    // the payload synchronously before the next _onData replaces
    // this.buf, so the view stays valid — the prior Buffer.from copy
    // was defensive against an async retention pattern that never
    // existed.
    frames.push({ type, channelId, payload: buf.subarray(offset + 7, offset + 4 + total) });
    offset += 4 + total;
  }
  return { frames, remaining: offset > 0 ? buf.subarray(offset) : buf };
}

export interface SpawnRequest {
  channel_id: number;
  cols: number;
  rows: number;
  shell_spec: ShellSpec;
}

export function encodeSpawnRequest(req: SpawnRequest): string {
  if (!Number.isInteger(req.channel_id) || req.channel_id <= 0 || req.channel_id > 0xffff
      || !Number.isInteger(req.cols) || req.cols <= 0 || req.cols > 0xffff
      || !Number.isInteger(req.rows) || req.rows <= 0 || req.rows > 0xffff
      || !isShellSpec(req.shell_spec)) {
    throw new RangeError("invalid keeper spawn request");
  }
  return JSON.stringify(req);
}

export function decodeSpawnRequest(payload: Buffer): SpawnRequest | null {
  try {
    const value: unknown = JSON.parse(payload.toString("utf8"));
    if (!value || typeof value !== "object" || Array.isArray(value)) return null;
    const keys = Object.keys(value);
    if (keys.length !== 4
        || !keys.every(key => key === "channel_id" || key === "cols"
          || key === "rows" || key === "shell_spec")) return null;
    const req = value as Partial<SpawnRequest>;
    if (typeof req.channel_id === "number" && Number.isInteger(req.channel_id)
        && req.channel_id > 0 && req.channel_id <= 0xffff
        && typeof req.cols === "number" && Number.isInteger(req.cols)
        && req.cols > 0 && req.cols <= 0xffff
        && typeof req.rows === "number" && Number.isInteger(req.rows)
        && req.rows > 0 && req.rows <= 0xffff
        && isShellSpec(req.shell_spec)) {
      return req as SpawnRequest;
    }
  } catch { /* fall through */ }
  return null;
}

export const KEEPER_MAX_INPUT_BYTES = 64 * 1024;
export const KEEPER_MAX_TERMINAL_DIMENSION = 0xffff;
export const KEEPER_MAX_HISTORY_RESIZE_RECORDS = 4096;

export function isSafeSequence(value: unknown): value is number {
  return typeof value === "number"
    && Number.isSafeInteger(value)
    && value > 0;
}

export function isDimension(value: unknown): value is number {
  return typeof value === "number"
    && Number.isInteger(value)
    && value > 0
    && value <= KEEPER_MAX_TERMINAL_DIMENSION;
}

export function isEmptyKeeperPayload(payload: Uint8Array): boolean {
  return payload.byteLength === 0;
}

export function writeSequence(out: Buffer, value: number, offset: number): void {
  if (!isSafeSequence(value)) throw new RangeError(`invalid keeper sequence: ${value}`);
  out.writeBigUInt64BE(BigInt(value), offset);
}

export function readSequence(payload: Uint8Array, offset: number): number | null {
  if (offset < 0 || offset + 8 > payload.byteLength) return null;
  const view = Buffer.from(payload.buffer, payload.byteOffset, payload.byteLength);
  const value = view.readBigUInt64BE(offset);
  if (value === 0n || value > BigInt(Number.MAX_SAFE_INTEGER)) return null;
  return Number(value);
}
