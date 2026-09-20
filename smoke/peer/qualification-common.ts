// Runtime peer qualification primitives own fixed raw payloads and bounded waits.
// Runtime-only uses these values unchanged; normal mode adds shared packet-codec transfers.
// The browser and native adapters share them without exposing SDP or endpoints.

import { createHash, randomBytes } from "node:crypto";
import type { TerminalPeerPacketLane } from "@roost/shared/terminal-peer";

export const QUALIFICATION_GENERATIONS = 20;
export const QUALIFICATION_MESSAGE_SIZES = [1, 1_024, 16_384] as const;

export const QUALIFICATION_DATA_CHANNEL_PROTOCOL = "roost.local-terminal.v1";
export const QUALIFICATION_CHANNELS: readonly {
  readonly id: 0 | 1 | 2;
  readonly label: string;
  readonly lane: TerminalPeerPacketLane;
}[] = [
  { id: 0, label: "roost-terminal-control-v1", lane: "control" },
  { id: 1, label: "roost-terminal-data-v1", lane: "terminal" },
  { id: 2, label: "roost-terminal-history-v1", lane: "history" },
];
export const NATIVE_UDP_PORT_RANGE_START = 49_152;
export const NATIVE_UDP_PORT_RANGE_END = 49_215;
export const OFFER_GATHERING_TIMEOUT_MS = 3_000;
export const CONNECTION_TIMEOUT_MS = 10_000;
export const CLOSE_TIMEOUT_MS = 5_000;

export type QualificationGeneration = number | "all";

export class PeerQualificationError extends Error {
  readonly stage: string;
  readonly generation: QualificationGeneration;

  constructor(stage: string, generation: QualificationGeneration, code: string) {
    super(`terminal peer qualification failed [stage=${stage} generation=${generation}]: ${code}`);
    this.name = "PeerQualificationError";
    this.stage = stage;
    this.generation = generation;
  }
}

export interface QualificationMessages {
  browserToNative: readonly Uint8Array[];
  nativeToBrowser: readonly Uint8Array[];
}

export interface Deferred<T> {
  readonly promise: Promise<T>;
  readonly settled: () => boolean;
  resolve(value: T): void;
  reject(reason: unknown): void;
}

export function qualificationFailure(
  stage: string,
  generation: QualificationGeneration,
  code: string,
): PeerQualificationError {
  return new PeerQualificationError(stage, generation, code);
}

export function asQualificationFailure(
  error: unknown,
  stage: string,
  generation: QualificationGeneration,
  code = "operation_failed",
): PeerQualificationError {
  return error instanceof PeerQualificationError
    ? error
    : qualificationFailure(stage, generation, code);
}

export async function runBrowserQualificationStep<T>(
  operation: Promise<T>,
  stage: string,
  generation: QualificationGeneration,
): Promise<T> {
  try {
    return await operation;
  } catch (error) {
    throw asQualificationFailure(error, stage, generation, "browser_operation_failed");
  }
}

export function createQualificationMessages(): QualificationMessages {
  return {
    browserToNative: QUALIFICATION_MESSAGE_SIZES.map((size) => new Uint8Array(randomBytes(size))),
    nativeToBrowser: QUALIFICATION_MESSAGE_SIZES.map((size) => new Uint8Array(randomBytes(size))),
  };
}

export function checksumHex(bytes: Uint8Array): string {
  return createHash("sha256").update(bytes).digest("hex");
}


export function encodeBase64(messages: readonly Uint8Array[]): readonly string[] {
  return messages.map((message) => Buffer.from(message.buffer, message.byteOffset, message.byteLength).toString("base64"));
}

export function hasIceCandidate(sdp: string | undefined): boolean {
  return sdp !== undefined && /(?:^|\r?\n)a=candidate:/u.test(sdp);
}

export function sha256FingerprintFromSdp(sdp: string): string | null {
  let fingerprint: string | null = null;
  for (const line of sdp.split(/\r?\n/u)) {
    if (!line.startsWith("a=fingerprint:")) continue;
    const match = /^a=fingerprint:sha-256\s+([0-9A-Fa-f:]+)\s*$/iu.exec(line);
    if (!match || fingerprint !== null) return null;
    fingerprint = normalizeSha256Fingerprint(match[1]!);
    if (fingerprint === null) return null;
  }
  return fingerprint;
}

export function normalizeSha256Fingerprint(value: string): string | null {
  const normalized = value.replaceAll(":", "").toLowerCase();
  return /^[0-9a-f]{64}$/u.test(normalized) ? normalized : null;
}

export function createDeferred<T>(): Deferred<T> {
  let settled = false;
  let resolvePromise!: (value: T) => void;
  let rejectPromise!: (reason: unknown) => void;
  const promise = new Promise<T>((resolve, reject) => {
    resolvePromise = (value) => {
      if (settled) return;
      settled = true;
      resolve(value);
    };
    rejectPromise = (reason) => {
      if (settled) return;
      settled = true;
      reject(reason);
    };
  });
  // Event callbacks can fail before their public waiter is reached. Mark the
  // rejection observed here while preserving it for the eventual awaiter.
  void promise.catch(() => undefined);
  return {
    promise,
    settled: () => settled,
    resolve: resolvePromise,
    reject: rejectPromise,
  };
}

export async function waitForQualification<T>(
  stage: string,
  generation: QualificationGeneration,
  timeoutMs: number,
  operation: Promise<T>,
): Promise<T> {
  let timeout: NodeJS.Timeout | undefined;
  try {
    return await Promise.race([
      operation,
      new Promise<never>((_, reject) => {
        timeout = setTimeout(() => reject(qualificationFailure(stage, generation, "timeout")), timeoutMs);
      }),
    ]);
  } finally {
    clearTimeout(timeout);
  }
}
