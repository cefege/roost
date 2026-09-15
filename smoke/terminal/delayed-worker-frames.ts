// WebSocket frame queueing for the delayed terminal worker-link fixture.
// The link proxy feeds each direction's TCP bytes here after a successful HTTP upgrade.
// An optional worker→coordinator filter decodes an owned payload copy only for an armed fault.
// Accepted messages retain their original masked WebSocket bytes.

import { Buffer } from "node:buffer";
import type { Socket } from "node:net";
import { fromBinary } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema,
  type CoordWorkerUp,
} from "../../apps/shared/src/gen/roost/v1/worker_transport_pb.ts";

const EMPTY_BUFFER: Buffer<ArrayBufferLike> = Buffer.alloc(0);
const MAX_FRAME_BYTES = 4 * 1024 * 1024;
const MAX_DIRECTION_QUEUE_BYTES = 8 * 1024 * 1024;
const BINARY_OPCODE = 0x2;

type FrameInspection = {
  frameBytes: number;
  headerBytes: number;
  payloadBytes: number;
  payloadOffset: number;
  maskOffset: number | null;
  opcode: number;
  fin: boolean;
  reservedBits: number;
} | "incomplete" | "oversized";
type QueuedFrame = { bytes: Buffer; dueAtMs: number };
type FilterDisposition = "forward" | "drop" | "failed";

export type WorkerFrameFilter = (frame: CoordWorkerUp) => boolean;

export class DelayedFrameStream {
  #pending = EMPTY_BUFFER;
  #frames: QueuedFrame[] = [];
  #queuedBytes = 0;
  #timer: NodeJS.Timeout | undefined;
  #writing = false;
  #finishing = false;
  #stopped = false;
  #onDrained: (() => void) | undefined;
  readonly #destination: Socket;
  readonly #oneWayDelayMs: 0 | 25 | 200;
  readonly #closePair: () => void;
  readonly #workerFrameFilter: WorkerFrameFilter | undefined;
  readonly #onFilterFailure: ((message: string) => void) | undefined;

  constructor(
    destination: Socket,
    oneWayDelayMs: 0 | 25 | 200,
    closePair: () => void,
    workerFrameFilter?: WorkerFrameFilter,
    onFilterFailure?: (message: string) => void,
  ) {
    this.#destination = destination;
    this.#oneWayDelayMs = oneWayDelayMs;
    this.#closePair = closePair;
    this.#workerFrameFilter = workerFrameFilter;
    this.#onFilterFailure = onFilterFailure;
  }

  receive(chunk: Buffer): void {
    if (this.#stopped || this.#finishing) return this.#closePair();
    let bytes = chunk;
    let bytesAreOwned = false;
    if (this.#pending.byteLength > 0) {
      bytes = Buffer.concat([this.#pending, chunk]);
      bytesAreOwned = true;
      this.#pending = EMPTY_BUFFER;
    }
    let offset = 0;
    while (offset < bytes.byteLength) {
      const inspection = inspectWebSocketFrame(bytes, offset);
      if (inspection === "oversized") return this.#closePair();
      if (inspection === "incomplete") {
        this.#holdPending(bytes.subarray(offset), bytesAreOwned);
        return;
      }
      const frame = bytes.subarray(offset, offset + inspection.frameBytes);
      const ownedFrame = bytesAreOwned ? frame : Buffer.from(frame);
      if (this.#workerFrameFilter) {
        const disposition = this.#filterWorkerFrame(ownedFrame, inspection);
        if (disposition === "failed") return;
        if (disposition === "drop") {
          offset += inspection.frameBytes;
          continue;
        }
      }
      if (!this.#enqueue(ownedFrame)) return;
      offset += inspection.frameBytes;
    }
    this.#pending = EMPTY_BUFFER;
  }

  finish(onDrained: () => void): boolean {
    if (this.#pending.byteLength > 0) return false;
    this.#finishing = true;
    this.#onDrained = onDrained;
    this.#finishIfDrained();
    return true;
  }

  stop(): void {
    this.#stopped = true;
    clearTimeout(this.#timer);
    this.#timer = undefined;
    this.#pending = EMPTY_BUFFER;
    this.#frames = [];
    this.#queuedBytes = 0;
    this.#onDrained = undefined;
  }

  #filterWorkerFrame(frame: Buffer, inspection: Exclude<FrameInspection, string>): FilterDisposition {
    if ((inspection.opcode & 0x8) !== 0) return "forward";
    if (
      inspection.opcode !== BINARY_OPCODE
      || !inspection.fin
      || inspection.reservedBits !== 0
      || inspection.maskOffset === null
    ) {
      return this.#failFilter("worker frame filter requires one uncompressed, unfragmented masked binary message");
    }
    const payload = copyUnmaskedPayload(frame, inspection);
    let decoded: CoordWorkerUp;
    try {
      decoded = fromBinary(CoordWorkerUpSchema, payload);
    } catch (error) {
      return this.#failFilter(`worker frame filter could not decode CoordWorkerUp: ${String(error)}`);
    }
    try {
      return this.#workerFrameFilter!(decoded) ? "forward" : "drop";
    } catch (error) {
      return this.#failFilter(`worker frame filter threw: ${String(error)}`);
    }
  }

  #failFilter(message: string): "failed" {
    this.#onFilterFailure?.(message);
    this.#closePair();
    return "failed";
  }

  #holdPending(bytes: Buffer, alreadyOwned: boolean): void {
    const pending = alreadyOwned ? bytes : Buffer.from(bytes);
    if (pending.byteLength > MAX_DIRECTION_QUEUE_BYTES - this.#queuedBytes) {
      this.#closePair();
      return;
    }
    this.#pending = pending;
  }

  #enqueue(ownedFrame: Buffer): boolean {
    if (ownedFrame.byteLength > MAX_DIRECTION_QUEUE_BYTES - this.#queuedBytes) {
      this.#closePair();
      return false;
    }
    this.#queuedBytes += ownedFrame.byteLength;
    this.#frames.push({ bytes: ownedFrame, dueAtMs: Date.now() + this.#oneWayDelayMs });
    this.#schedule();
    return true;
  }

  #schedule(): void {
    if (this.#stopped || this.#writing || this.#timer || this.#frames.length === 0) return;
    const delayMs = Math.max(0, this.#frames[0]!.dueAtMs - Date.now());
    if (delayMs === 0) return this.#writeHead();
    this.#timer = setTimeout(() => {
      this.#timer = undefined;
      this.#writeHead();
    }, delayMs);
  }

  #writeHead(): void {
    if (this.#stopped || this.#writing) return;
    const frame = this.#frames[0];
    if (!frame) return this.#finishIfDrained();
    if (this.#destination.destroyed || !this.#destination.writable) return this.#closePair();
    this.#writing = true;
    try {
      this.#destination.write(frame.bytes, (error) => {
        if (this.#stopped) return;
        this.#writing = false;
        if (error) return this.#closePair();
        const sent = this.#frames.shift();
        if (!sent) return this.#closePair();
        this.#queuedBytes -= sent.bytes.byteLength;
        this.#finishIfDrained();
        this.#schedule();
      });
    } catch {
      this.#writing = false;
      this.#closePair();
    }
  }

  #finishIfDrained(): void {
    if (!this.#finishing || this.#writing || this.#timer || this.#frames.length > 0) return;
    const onDrained = this.#onDrained;
    this.#onDrained = undefined;
    if (onDrained) onDrained();
  }
}

function inspectWebSocketFrame(bytes: Buffer, offset: number): FrameInspection {
  const available = bytes.byteLength - offset;
  if (available < 2) return "incomplete";
  const firstByte = bytes[offset]!;
  const secondByte = bytes[offset + 1]!;
  const lengthMarker = secondByte & 0x7f;
  let headerBytes = 2;
  let payloadBytes: number;
  if (lengthMarker === 126) {
    if (available < 4) return "incomplete";
    payloadBytes = bytes.readUInt16BE(offset + 2);
    headerBytes += 2;
  } else if (lengthMarker === 127) {
    if (available < 10) return "incomplete";
    const highBits = bytes.readUInt32BE(offset + 2);
    const lowBits = bytes.readUInt32BE(offset + 6);
    if (highBits !== 0 || lowBits > MAX_FRAME_BYTES) return "oversized";
    payloadBytes = lowBits;
    headerBytes += 8;
  } else {
    payloadBytes = lengthMarker;
  }
  const maskOffset = (secondByte & 0x80) === 0 ? null : headerBytes;
  if (maskOffset !== null) headerBytes += 4;
  if (headerBytes + payloadBytes > MAX_FRAME_BYTES) return "oversized";
  if (available < headerBytes + payloadBytes) return "incomplete";
  return {
    frameBytes: headerBytes + payloadBytes,
    headerBytes,
    payloadBytes,
    payloadOffset: headerBytes,
    maskOffset,
    opcode: firstByte & 0x0f,
    fin: (firstByte & 0x80) !== 0,
    reservedBits: firstByte & 0x70,
  };
}

function copyUnmaskedPayload(
  frame: Buffer,
  inspection: Exclude<FrameInspection, string>,
): Buffer {
  const payload = Buffer.allocUnsafe(inspection.payloadBytes);
  const maskOffset = inspection.maskOffset!;
  for (let index = 0; index < inspection.payloadBytes; index++) {
    payload[index] = frame[inspection.payloadOffset + index]! ^ frame[maskOffset + (index & 3)]!;
  }
  return payload;
}
