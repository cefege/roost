// Keeper protocol — resize control frames, authoritative terminal state, and
// ordered history records. Wire format and version history: ./protocol.ts.
import {
  isDimension, isSafeSequence, KEEPER_MAX_HISTORY_RESIZE_RECORDS, MuxFrameType,
  readSequence, writeSequence,
} from "./protocol-envelope.ts";

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

/** Authoritative per-channel resize state, read straight from the keeper's live
 *  channel rather than from its bounded retained markers. `highestResizeSeq`
 *  counts every sequence the keeper CONSUMED (applied or rejected), so the
 *  worker's next allocation must exceed it even when the marker that recorded
 *  it has already been evicted. */
export interface KeeperTerminalState {
  headSeq: number;
  cols: number;
  rows: number;
  highestResizeSeq: number;
  /** Highest sequence whose cached result is an ACK; 0 when none applied. */
  appliedResizeSeq: number;
}

const TERMINAL_STATE_FORMAT_VERSION = 1;
const TERMINAL_STATE_BYTES = 33;

/** These three fields are COUNTERS, not sequences: zero is the truthful value
 *  for a channel that has produced no output or consumed no resize. The shared
 *  sequence codec rejects zero (a real command sequence starts at 1), so the
 *  terminal state carries its own reader rather than encoding "nothing yet" as a
 *  decode failure. */
function readCounter(view: Buffer, offset: number): number | null {
  const value = view.readBigUInt64BE(offset);
  return value > BigInt(Number.MAX_SAFE_INTEGER) ? null : Number(value);
}

function isCounter(value: number): boolean {
  return Number.isSafeInteger(value) && value >= 0;
}

export function encodeKeeperTerminalState(state: KeeperTerminalState): Buffer {
  if (!isCounter(state.headSeq) || !isCounter(state.highestResizeSeq)
      || !isCounter(state.appliedResizeSeq)
      || !isDimension(state.cols) || !isDimension(state.rows)
      || state.appliedResizeSeq > state.highestResizeSeq) {
    throw new RangeError("invalid keeper terminal state");
  }
  const out = Buffer.allocUnsafe(TERMINAL_STATE_BYTES);
  out[0] = TERMINAL_STATE_FORMAT_VERSION;
  out.writeBigUInt64BE(BigInt(state.headSeq), 1);
  out.writeBigUInt64BE(BigInt(state.highestResizeSeq), 9);
  out.writeBigUInt64BE(BigInt(state.appliedResizeSeq), 17);
  out.writeUInt32BE(state.cols, 25);
  out.writeUInt32BE(state.rows, 29);
  return out;
}

export function decodeKeeperTerminalState(payload: Uint8Array): KeeperTerminalState | null {
  if (payload.byteLength !== TERMINAL_STATE_BYTES) return null;
  const view = Buffer.from(payload.buffer, payload.byteOffset, payload.byteLength);
  if (view[0] !== TERMINAL_STATE_FORMAT_VERSION) return null;
  const headSeq = readCounter(view, 1);
  const highestResizeSeq = readCounter(view, 9);
  const appliedResizeSeq = readCounter(view, 17);
  if (headSeq === null || highestResizeSeq === null || appliedResizeSeq === null) return null;
  if (appliedResizeSeq > highestResizeSeq) return null;
  const cols = view.readUInt32BE(25);
  const rows = view.readUInt32BE(29);
  if (!isDimension(cols) || !isDimension(rows)) return null;
  return { headSeq, cols, rows, highestResizeSeq, appliedResizeSeq };
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
