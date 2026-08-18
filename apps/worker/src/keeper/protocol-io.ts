// Keeper protocol — hello handshake (feature negotiation) and PtyIn typed input
// frames. Wire format and version history: ./protocol.ts.
import { z } from "zod";
import {
  allocateMuxFrame, isSafeSequence, KEEPER_MAX_INPUT_BYTES, MuxFrameType,
  readSequence, writeSequence,
} from "./protocol-envelope.ts";

export const KeeperFeature = {
  OrderedHistory: "ordered_history_v1",
  AcknowledgedInput: "acknowledged_input_v1",
  AcknowledgedResize: "acknowledged_resize_v1",
  TerminalState: "terminal_state_v1",
} as const;

export type KeeperFeature = typeof KeeperFeature[keyof typeof KeeperFeature];

export const SUPPORTED_KEEPER_FEATURES: readonly KeeperFeature[] = [
  KeeperFeature.OrderedHistory,
  KeeperFeature.AcknowledgedInput,
  KeeperFeature.AcknowledgedResize,
  KeeperFeature.TerminalState,
];

/** Features whose ABSENCE makes a surviving keeper unusable. Deliberately NOT
 *  the same list as above: an incompatible keeper is KILLED, taking every live
 *  PTY with it, so a feature the worker can degrade gracefully must never appear
 *  here. `TerminalState` is absent because the resize owner falls back to a
 *  status probe of the last written sequence when the keeper predates it. */
export const REQUIRED_KEEPER_FEATURES: readonly KeeperFeature[] = [
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

export interface PtyInRequest {
  inputSeq: number;
  bytes: Uint8Array;
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
