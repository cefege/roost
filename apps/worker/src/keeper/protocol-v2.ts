// T2.1 — multiplexed keeper protocol. A single keeper process hosts N
// PTYs; each frame carries a channel_id so the worker side can demux.
//
// Frame format:
//   [4-byte BE uint32 total_length] (covers everything after these 4 bytes)
//   [1-byte type tag]
//   [2-byte BE uint16 channel_id]   (0 = control / global)
//   [payload bytes]
//
// Stable, never renumber the type tags.
import { z } from "zod";
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

  // Authenticated administrative shutdown. The endpoint layer authenticates
  // Hello before either frame can be dispatched.
  Shutdown          = 0xE8, // client → keeper: empty payload (channel=0)
  ShutdownAck       = 0xE9, // keeper → client: empty payload (channel=0)
}

/** Wire-protocol version. BUMP whenever:
 *  - any existing frame's payload JSON shape changes
 *  - a frame's tag number is reassigned (never do this; add a new tag)
 *  - the encoding of any frame changes (e.g. switching JSON → binary)
 *
 *  A bump makes a running keeper incompatible. Authentication still succeeds
 *  so administrative tooling can report or explicitly shut it down, but the
 *  worker must not dispatch application commands across the mismatch.
 *  Backwards-compatible additive tags may instead be feature-negotiated.
 *
 *  Bump log:
 *    1 — initial Hello/HelloResp handshake (2026-06-18)
 *    2 — authenticated Hello, ShellSpec Spawn, ordered history and typed IO */
export const KEEPER_PROTOCOL_VERSION = 2;

export interface MuxFrame {
  type: MuxFrameType;
  channelId: number;
  payload: Buffer;
}

export const KEEPER_MAX_MUX_FRAME_BYTES = 16 * 1024 * 1024;

function allocateMuxFrame(type: MuxFrameType, channelId: number, payloadLength: number): Buffer {
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

export const KeeperFeature = {
  OrderedHistory: "ordered_history_v1",
  AcknowledgedInput: "acknowledged_input_v1",
  AcknowledgedResize: "acknowledged_resize_v1",
} as const;

export type KeeperFeature = typeof KeeperFeature[keyof typeof KeeperFeature];

export const SUPPORTED_KEEPER_FEATURES: readonly KeeperFeature[] = [
  KeeperFeature.OrderedHistory,
  KeeperFeature.AcknowledgedInput,
  KeeperFeature.AcknowledgedResize,
];

const HelloFeatureList = z.array(z.string().min(1).max(64))
  .max(32)
  .refine(features => new Set(features).size === features.length);

export const KeeperHelloRequestSchema = z.object({
  version: z.number().int().positive().max(Number.MAX_SAFE_INTEGER),
  capability: z.string().min(1).max(512),
  features: HelloFeatureList,
  pid: z.number().int().positive().max(Number.MAX_SAFE_INTEGER).optional(),
  process_epoch: z.string().min(1).max(128).optional(),
}).strict();

export type KeeperHelloRequest = z.infer<typeof KeeperHelloRequestSchema>;

export const KeeperHelloResponseSchema = z.object({
  version: z.number().int().positive().max(Number.MAX_SAFE_INTEGER),
  authenticated: z.literal(true),
  features: HelloFeatureList,
  build: z.string().max(256).optional(),
  pid: z.number().int().positive().max(Number.MAX_SAFE_INTEGER).optional(),
  process_epoch: z.string().min(1).max(128).optional(),
}).strict();

export type KeeperHelloResponse = z.infer<typeof KeeperHelloResponseSchema>;

function isSafeSequence(value: unknown): value is number {
  return typeof value === "number"
    && Number.isSafeInteger(value)
    && value > 0;
}

function isDimension(value: unknown): value is number {
  return typeof value === "number"
    && Number.isInteger(value)
    && value > 0
    && value <= KEEPER_MAX_TERMINAL_DIMENSION;
}


export function encodeKeeperHelloRequest(request: KeeperHelloRequest): string {
  return JSON.stringify(KeeperHelloRequestSchema.parse(request));
}

export function decodeKeeperHelloRequest(payload: Uint8Array): KeeperHelloRequest | null {
  try {
    const parsed = KeeperHelloRequestSchema.safeParse(JSON.parse(Buffer.from(
      payload.buffer,
      payload.byteOffset,
      payload.byteLength,
    ).toString("utf8")));
    return parsed.success ? parsed.data : null;
  } catch {
    return null;
  }
}

export function encodeKeeperHelloResponse(response: KeeperHelloResponse): string {
  return JSON.stringify(KeeperHelloResponseSchema.parse(response));
}

export function decodeKeeperHelloResponse(payload: Uint8Array): KeeperHelloResponse | null {
  try {
    const parsed = KeeperHelloResponseSchema.safeParse(JSON.parse(Buffer.from(
      payload.buffer,
      payload.byteOffset,
      payload.byteLength,
    ).toString("utf8")));
    return parsed.success ? parsed.data : null;
  } catch {
    return null;
  }
}

export function negotiateKeeperFeatures(requested: readonly string[]): KeeperFeature[] {
  const requestedSet = new Set(requested);
  return SUPPORTED_KEEPER_FEATURES.filter(feature => requestedSet.has(feature));
}

export function isEmptyKeeperPayload(payload: Uint8Array): boolean {
  return payload.byteLength === 0;
}

function writeSequence(out: Buffer, value: number, offset: number): void {
  if (!isSafeSequence(value)) throw new RangeError(`invalid keeper sequence: ${value}`);
  out.writeBigUInt64BE(BigInt(value), offset);
}

function readSequence(payload: Uint8Array, offset: number): number | null {
  if (offset < 0 || offset + 8 > payload.byteLength) return null;
  const view = Buffer.from(payload.buffer, payload.byteOffset, payload.byteLength);
  const value = view.readBigUInt64BE(offset);
  if (value === 0n || value > BigInt(Number.MAX_SAFE_INTEGER)) return null;
  return Number(value);
}

export interface PtyInRequest {
  inputSeq: number;
  bytes: Uint8Array;
}

export function encodePtyInRequest(request: PtyInRequest): Buffer {
  if (!isSafeSequence(request.inputSeq)
      || request.bytes.byteLength === 0
      || request.bytes.byteLength > KEEPER_MAX_INPUT_BYTES) {
    throw new RangeError("invalid keeper input request");
  }
  const out = Buffer.allocUnsafe(8 + request.bytes.byteLength);
  writeSequence(out, request.inputSeq, 0);
  out.set(request.bytes, 8);
  return out;
}

/** Hot-path input encoder: request header and defensive byte copy land
 * directly in the final mux frame allocation. */
export function encodePtyInRequestFrame(channelId: number, request: PtyInRequest): Buffer {
  if (!isSafeSequence(request.inputSeq)
      || request.bytes.byteLength === 0
      || request.bytes.byteLength > KEEPER_MAX_INPUT_BYTES) {
    throw new RangeError("invalid keeper input request");
  }
  const out = allocateMuxFrame(MuxFrameType.PtyInRequest, channelId, 8 + request.bytes.byteLength);
  writeSequence(out, request.inputSeq, 7);
  out.set(request.bytes, 15);
  return out;
}

export function decodePtyInRequest(payload: Uint8Array): PtyInRequest | null {
  if (payload.byteLength <= 8 || payload.byteLength > 8 + KEEPER_MAX_INPUT_BYTES) return null;
  const inputSeq = readSequence(payload, 0);
  if (inputSeq === null) return null;
  return { inputSeq, bytes: payload.subarray(8) };
}

export type PtyInFailureReason =
  | "channel_missing"
  | "channel_exited"
  | "terminal_missing"
  | "queue_full"
  | "deadline"
  | "write_error"
  | "invalid_write_count"
  | "invalid_request"
  | "unsupported"
  | "disconnected";

export type PtyInAmbiguousReason =
  | "channel_exited"
  | "deadline"
  | "write_error"
  | "invalid_write_count";

export interface PtyInAck {
  kind: "ack";
  inputSeq: number;
  writtenBytes: number;
}

export interface PtyInReject {
  kind: "reject";
  inputSeq: number;
  writtenBytes: 0;
  reason: PtyInFailureReason;
}

export interface PtyInAmbiguous {
  kind: "ambiguous";
  inputSeq: number;
  writtenBytes: number;
  reason: PtyInAmbiguousReason;
}

export type PtyInWireResult = PtyInAck | PtyInReject | PtyInAmbiguous;

export type KeeperInputResult = PtyInWireResult | {
  kind: "ambiguous";
  inputSeq: number;
  writtenBytes: null;
  reason: "disconnected" | "timeout" | "protocol_error";
};

const PTY_IN_REASON_TO_CODE: Readonly<Record<PtyInFailureReason, number>> = {
  channel_missing: 1,
  channel_exited: 2,
  terminal_missing: 3,
  queue_full: 4,
  deadline: 5,
  write_error: 6,
  invalid_write_count: 7,
  invalid_request: 8,
  unsupported: 9,
  disconnected: 10,
};

function decodePtyInReason(code: number): PtyInFailureReason | null {
  switch (code) {
    case 1: return "channel_missing";
    case 2: return "channel_exited";
    case 3: return "terminal_missing";
    case 4: return "queue_full";
    case 5: return "deadline";
    case 6: return "write_error";
    case 7: return "invalid_write_count";
    case 8: return "invalid_request";
    case 9: return "unsupported";
    case 10: return "disconnected";
    default: return null;
  }
}

export function encodePtyInResult(result: PtyInWireResult): Buffer {
  if (!Number.isInteger(result.writtenBytes)
      || result.writtenBytes < 0
      || result.writtenBytes > KEEPER_MAX_INPUT_BYTES
      || (result.kind === "ack" && result.writtenBytes === 0)
      || (result.kind === "reject" && result.writtenBytes !== 0)
      || (result.kind === "ambiguous"
        && (result.writtenBytes === 0
          || (result.reason !== "channel_exited"
            && result.reason !== "deadline"
            && result.reason !== "write_error"
            && result.reason !== "invalid_write_count")))) {
    throw new RangeError("invalid keeper input result");
  }
  const out = Buffer.allocUnsafe(result.kind === "ack" ? 12 : 13);
  writeSequence(out, result.inputSeq, 0);
  out.writeUInt32BE(result.writtenBytes, 8);
  if (result.kind !== "ack") out[12] = PTY_IN_REASON_TO_CODE[result.reason];
  return out;
}

export function decodePtyInResult(
  frameType: MuxFrameType,
  payload: Uint8Array,
): PtyInWireResult | null {
  const kind = frameType === MuxFrameType.PtyInAck
    ? "ack"
    : frameType === MuxFrameType.PtyInReject
      ? "reject"
      : frameType === MuxFrameType.PtyInAmbiguous
        ? "ambiguous"
        : null;
  if (kind === null || payload.byteLength !== (kind === "ack" ? 12 : 13)) return null;
  const inputSeq = readSequence(payload, 0);
  if (inputSeq === null) return null;
  const view = Buffer.from(payload.buffer, payload.byteOffset, payload.byteLength);
  const writtenBytes = view.readUInt32BE(8);
  if (writtenBytes > KEEPER_MAX_INPUT_BYTES) return null;
  if (kind === "ack") {
    return writtenBytes > 0 ? { kind, inputSeq, writtenBytes } : null;
  }
  const reason = decodePtyInReason(view[12]!);
  if (!reason) return null;
  if (kind === "reject") {
    return writtenBytes === 0 ? { kind, inputSeq, writtenBytes: 0, reason } : null;
  }
  if (writtenBytes === 0
      || (reason !== "channel_exited"
        && reason !== "deadline"
        && reason !== "write_error"
        && reason !== "invalid_write_count")) return null;
  return { kind, inputSeq, writtenBytes, reason };
}

export interface ResizeRequest {
  seq: number;
  cols: number;
  rows: number;
}

export interface ResizeStatusQuery {
  seq: number;
}

export type ResizeRejectReason =
  | "channel_missing"
  | "channel_exited"
  | "terminal_missing"
  | "resize_error"
  | "stale_sequence"
  | "unknown_sequence"
  | "invalid_request"
  | "unsupported"
  | "disconnected";

export interface ResizeAck {
  kind: "ack";
  seq: number;
  cols: number;
  rows: number;
}

export interface ResizeReject {
  kind: "reject";
  seq: number;
  reason: ResizeRejectReason;
}

export type ResizeWireResult = ResizeAck | ResizeReject;

export type KeeperResizeResult = ResizeWireResult | {
  kind: "unknown";
  seq: number;
  reason: "disconnected" | "timeout" | "protocol_error";
};

const RESIZE_REASON_TO_CODE: Readonly<Record<ResizeRejectReason, number>> = {
  channel_missing: 1,
  channel_exited: 2,
  terminal_missing: 3,
  resize_error: 4,
  stale_sequence: 5,
  unknown_sequence: 6,
  invalid_request: 7,
  unsupported: 8,
  disconnected: 9,
};

function decodeResizeReason(code: number): ResizeRejectReason | null {
  switch (code) {
    case 1: return "channel_missing";
    case 2: return "channel_exited";
    case 3: return "terminal_missing";
    case 4: return "resize_error";
    case 5: return "stale_sequence";
    case 6: return "unknown_sequence";
    case 7: return "invalid_request";
    case 8: return "unsupported";
    case 9: return "disconnected";
    default: return null;
  }
}

export function encodeResizeRequest(request: ResizeRequest): Buffer {
  if (!isSafeSequence(request.seq) || !isDimension(request.cols) || !isDimension(request.rows)) {
    throw new RangeError("invalid keeper resize request");
  }
  const out = Buffer.allocUnsafe(16);
  writeSequence(out, request.seq, 0);
  out.writeUInt32BE(request.cols, 8);
  out.writeUInt32BE(request.rows, 12);
  return out;
}

export function decodeResizeRequest(payload: Uint8Array): ResizeRequest | null {
  if (payload.byteLength !== 16) return null;
  const seq = readSequence(payload, 0);
  if (seq === null) return null;
  const view = Buffer.from(payload.buffer, payload.byteOffset, payload.byteLength);
  const cols = view.readUInt32BE(8);
  const rows = view.readUInt32BE(12);
  return isDimension(cols) && isDimension(rows) ? { seq, cols, rows } : null;
}

export function encodeResizeStatusQuery(query: ResizeStatusQuery): Buffer {
  const out = Buffer.allocUnsafe(8);
  writeSequence(out, query.seq, 0);
  return out;
}

export function decodeResizeStatusQuery(payload: Uint8Array): ResizeStatusQuery | null {
  if (payload.byteLength !== 8) return null;
  const seq = readSequence(payload, 0);
  return seq === null ? null : { seq };
}

export function encodeResizeResult(result: ResizeWireResult): Buffer {
  if (result.kind === "ack") return encodeResizeRequest(result);
  const out = Buffer.allocUnsafe(9);
  writeSequence(out, result.seq, 0);
  out[8] = RESIZE_REASON_TO_CODE[result.reason];
  return out;
}

export function decodeResizeResult(
  frameType: MuxFrameType,
  payload: Uint8Array,
): ResizeWireResult | null {
  if (frameType === MuxFrameType.ResizeAck) {
    const ack = decodeResizeRequest(payload);
    return ack ? { kind: "ack", ...ack } : null;
  }
  if (frameType !== MuxFrameType.ResizeReject || payload.byteLength !== 9) return null;
  const seq = readSequence(payload, 0);
  if (seq === null) return null;
  const reason = decodeResizeReason(payload[8]!);
  return reason ? { kind: "reject", seq, reason } : null;
}

export type KeeperHistoryRecord =
  | { kind: "output"; bytes: Uint8Array }
  | { kind: "resize"; seq: number; cols: number; rows: number };

export interface KeeperHistoryRecords {
  headSeq: number;
  baseCols: number;
  baseRows: number;
  records: KeeperHistoryRecord[];
}

const HISTORY_FORMAT_VERSION = 1;
const HISTORY_HEADER_BYTES = 21;
const HISTORY_OUTPUT_HEADER_BYTES = 5;
const HISTORY_RESIZE_BYTES = 17;
const HISTORY_OUTPUT_TAG = 1;
const HISTORY_RESIZE_TAG = 2;
const MAX_HISTORY_RECORDS = KEEPER_MAX_HISTORY_RESIZE_RECORDS * 2 + 1;
const MAX_HISTORY_PAYLOAD_BYTES = 2 * 1024 * 1024;

export function encodeKeeperHistoryRecords(history: KeeperHistoryRecords): Buffer {
  if (!Number.isSafeInteger(history.headSeq) || history.headSeq < 0
      || !isDimension(history.baseCols) || !isDimension(history.baseRows)
      || history.records.length > MAX_HISTORY_RECORDS) {
    throw new RangeError("invalid keeper history");
  }
  let payloadBytes = HISTORY_HEADER_BYTES;
  let rawBytes = 0;
  for (const record of history.records) {
    if (record.kind === "output") {
      if (record.bytes.byteLength === 0) throw new RangeError("empty keeper output record");
      rawBytes += record.bytes.byteLength;
      payloadBytes += HISTORY_OUTPUT_HEADER_BYTES + record.bytes.byteLength;
    } else {
      if (!Number.isSafeInteger(record.seq) || record.seq < 0
          || !isDimension(record.cols) || !isDimension(record.rows)) {
        throw new RangeError("invalid keeper resize history record");
      }
      payloadBytes += HISTORY_RESIZE_BYTES;
    }
    if (!Number.isSafeInteger(rawBytes) || payloadBytes > MAX_HISTORY_PAYLOAD_BYTES) {
      throw new RangeError("keeper history payload exceeds bounds");
    }
  }
  if (rawBytes > history.headSeq) throw new RangeError("keeper history exceeds head sequence");

  const out = Buffer.allocUnsafe(payloadBytes);
  out[0] = HISTORY_FORMAT_VERSION;
  out.writeBigUInt64BE(BigInt(history.headSeq), 1);
  out.writeUInt32BE(history.baseCols, 9);
  out.writeUInt32BE(history.baseRows, 13);
  out.writeUInt32BE(history.records.length, 17);
  let offset = HISTORY_HEADER_BYTES;
  for (const record of history.records) {
    if (record.kind === "output") {
      out[offset] = HISTORY_OUTPUT_TAG;
      out.writeUInt32BE(record.bytes.byteLength, offset + 1);
      out.set(record.bytes, offset + HISTORY_OUTPUT_HEADER_BYTES);
      offset += HISTORY_OUTPUT_HEADER_BYTES + record.bytes.byteLength;
    } else {
      out[offset] = HISTORY_RESIZE_TAG;
      out.writeBigUInt64BE(BigInt(record.seq), offset + 1);
      out.writeUInt32BE(record.cols, offset + 9);
      out.writeUInt32BE(record.rows, offset + 13);
      offset += HISTORY_RESIZE_BYTES;
    }
  }
  return out;
}

export function decodeKeeperHistoryRecords(payload: Uint8Array): KeeperHistoryRecords | null {
  if (payload.byteLength < HISTORY_HEADER_BYTES || payload.byteLength > MAX_HISTORY_PAYLOAD_BYTES) {
    return null;
  }
  const view = Buffer.from(payload.buffer, payload.byteOffset, payload.byteLength);
  if (view[0] !== HISTORY_FORMAT_VERSION) return null;
  const head = view.readBigUInt64BE(1);
  if (head > BigInt(Number.MAX_SAFE_INTEGER)) return null;
  const headSeq = Number(head);
  const baseCols = view.readUInt32BE(9);
  const baseRows = view.readUInt32BE(13);
  const recordCount = view.readUInt32BE(17);
  if (!isDimension(baseCols) || !isDimension(baseRows) || recordCount > MAX_HISTORY_RECORDS) {
    return null;
  }

  const records: KeeperHistoryRecord[] = [];
  let rawBytes = 0;
  let offset = HISTORY_HEADER_BYTES;
  for (let i = 0; i < recordCount; i++) {
    if (offset >= view.byteLength) return null;
    const tag = view[offset];
    if (tag === HISTORY_OUTPUT_TAG) {
      if (offset + HISTORY_OUTPUT_HEADER_BYTES > view.byteLength) return null;
      const length = view.readUInt32BE(offset + 1);
      if (length === 0 || offset + HISTORY_OUTPUT_HEADER_BYTES + length > view.byteLength) return null;
      const start = offset + HISTORY_OUTPUT_HEADER_BYTES;
      records.push({ kind: "output", bytes: new Uint8Array(view.subarray(start, start + length)) });
      rawBytes += length;
      if (!Number.isSafeInteger(rawBytes) || rawBytes > headSeq) return null;
      offset = start + length;
      continue;
    }
    if (tag === HISTORY_RESIZE_TAG) {
      if (offset + HISTORY_RESIZE_BYTES > view.byteLength) return null;
      const rawSeq = view.readBigUInt64BE(offset + 1);
      const cols = view.readUInt32BE(offset + 9);
      const rows = view.readUInt32BE(offset + 13);
      if (rawSeq > BigInt(Number.MAX_SAFE_INTEGER)
          || !isDimension(cols) || !isDimension(rows)) return null;
      const seq = Number(rawSeq);
      records.push({ kind: "resize", seq, cols, rows });
      offset += HISTORY_RESIZE_BYTES;
      continue;
    }
    return null;
  }
  if (offset !== view.byteLength) return null;
  return { headSeq, baseCols, baseRows, records };
}
