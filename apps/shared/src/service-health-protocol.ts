import { z } from "zod";
import {
  LOCAL_ENDPOINT_UNAUTHENTICATED_MAX_BYTES,
} from "./local-endpoint.ts";

export const SERVICE_HEALTH_PROTOCOL_VERSION = 1;
const SERVICE_HEALTH_GENERATION_PATTERN = /^[a-f0-9]{64}$/;
const MAX_METADATA_LENGTH = 256;
const MAX_URL_LENGTH = 4_096;
const MAX_FRAME_PAYLOAD_BYTES = LOCAL_ENDPOINT_UNAUTHENTICATED_MAX_BYTES - 4;

const ServiceHealthRoleSchema = z.enum(["coordinator", "worker"]);
const HealthMetadata = z.string().min(1).max(MAX_METADATA_LENGTH);
const HealthUrl = z.string().min(1).max(MAX_URL_LENGTH);
const CommonHealthShape = {
  version: HealthMetadata,
  build: HealthMetadata,
  processEpoch: HealthMetadata,
  ready: z.boolean(),
};

export type ServiceHealthRole = z.infer<typeof ServiceHealthRoleSchema>;

export interface ServiceHealthCommon {
  role: ServiceHealthRole;
  version: string;
  build: string;
  processEpoch: string;
  ready: boolean;
}

export interface CoordinatorServiceHealth extends ServiceHealthCommon {
  role: "coordinator";
  dbReady: boolean;
  listenerReady: boolean;
  advertisedUrl?: string;
}

export interface WorkerServiceHealth extends ServiceHealthCommon {
  role: "worker";
  targetLinkReady: boolean;
  coordinatorUrl: string;
}

export const CoordinatorServiceHealthSchema: z.ZodType<CoordinatorServiceHealth> = z.object({
  ...CommonHealthShape,
  role: z.literal("coordinator"),
  dbReady: z.boolean(),
  listenerReady: z.boolean(),
  advertisedUrl: HealthUrl.optional(),
}).strict().refine(
  (status) => !status.ready || (status.dbReady && status.listenerReady),
  { message: "ready coordinator health requires its database and listener" },
);

export const WorkerServiceHealthSchema: z.ZodType<WorkerServiceHealth> = z.object({
  ...CommonHealthShape,
  role: z.literal("worker"),
  targetLinkReady: z.boolean(),
  coordinatorUrl: HealthUrl,
}).strict().refine(
  (status) => !status.ready || status.targetLinkReady,
  { message: "ready worker health requires its coordinator target link" },
);

export type ServiceHealthStatus = CoordinatorServiceHealth | WorkerServiceHealth;
export type ServiceHealthStatusFor<R extends ServiceHealthRole> = Extract<ServiceHealthStatus, { role: R }>;

export const ServiceHealthProbeCapabilitySchema = z.object({
  capability: z.string().regex(SERVICE_HEALTH_GENERATION_PATTERN),
}).passthrough();

export const ServiceHealthProbeRequestSchema = z.object({
  protocolVersion: z.literal(SERVICE_HEALTH_PROTOCOL_VERSION),
  kind: z.literal("probe"),
  role: ServiceHealthRoleSchema,
  generation: z.string().regex(SERVICE_HEALTH_GENERATION_PATTERN),
  capability: z.string().regex(SERVICE_HEALTH_GENERATION_PATTERN),
}).strict();
export type ServiceHealthProbeRequest = z.infer<typeof ServiceHealthProbeRequestSchema>;

export const ServiceHealthProbeResponseSchema = z.object({
  protocolVersion: z.literal(SERVICE_HEALTH_PROTOCOL_VERSION),
  kind: z.literal("status"),
  role: ServiceHealthRoleSchema,
  generation: z.string().regex(SERVICE_HEALTH_GENERATION_PATTERN),
  status: z.union([CoordinatorServiceHealthSchema, WorkerServiceHealthSchema]),
}).strict();
export type ServiceHealthProbeResponse = z.infer<typeof ServiceHealthProbeResponseSchema>;

export function validateServiceHealthStatus<R extends ServiceHealthRole>(
  role: R,
  value: unknown,
): ServiceHealthStatusFor<R> {
  const result = role === "coordinator"
    ? CoordinatorServiceHealthSchema.safeParse(value)
    : WorkerServiceHealthSchema.safeParse(value);
  if (!result.success || result.data.role !== role) {
    throw new Error(`invalid ${role} service health status`);
  }
  return result.data as unknown as ServiceHealthStatusFor<R>;
}

export function encodeServiceHealthFrame(value: unknown): Buffer {
  const json = JSON.stringify(value);
  const payloadLength = Buffer.byteLength(json);
  if (payloadLength > MAX_FRAME_PAYLOAD_BYTES) throw new Error("service health frame is too large");
  const frame = Buffer.allocUnsafe(payloadLength + 4);
  frame.writeUInt32LE(payloadLength, 0);
  frame.write(json, 4, payloadLength, "utf8");
  return frame;
}

export function decodeServiceHealthFrame(payload: Buffer): unknown {
  const text = new TextDecoder("utf-8", { fatal: true }).decode(payload);
  return JSON.parse(text) as unknown;
}

export type ServiceHealthFrameProgress =
  | { complete: false }
  | { complete: true; payload: Buffer };

export class ServiceHealthFrameAccumulator {
  readonly #chunks: Buffer[] = [];
  #totalBytes = 0;
  #payloadLength: number | undefined;
  #complete = false;

  push(chunk: Buffer): ServiceHealthFrameProgress {
    if (this.#complete || chunk.byteLength === 0) throw new Error("invalid service health frame");
    this.#totalBytes += chunk.byteLength;
    if (this.#totalBytes > LOCAL_ENDPOINT_UNAUTHENTICATED_MAX_BYTES) {
      throw new Error("service health frame is too large");
    }
    this.#chunks.push(chunk);

    if (this.#payloadLength === undefined && this.#totalBytes >= 4) {
      const header = Buffer.allocUnsafe(4);
      let copied = 0;
      for (const part of this.#chunks) {
        const count = Math.min(4 - copied, part.byteLength);
        part.copy(header, copied, 0, count);
        copied += count;
        if (copied === 4) break;
      }
      this.#payloadLength = header.readUInt32LE(0);
      if (this.#payloadLength === 0 || this.#payloadLength > MAX_FRAME_PAYLOAD_BYTES) {
        throw new Error("invalid service health frame length");
      }
    }

    if (this.#payloadLength === undefined || this.#totalBytes < this.#payloadLength + 4) {
      return { complete: false };
    }
    if (this.#totalBytes !== this.#payloadLength + 4) {
      throw new Error("service health connection contained multiple frames");
    }

    this.#complete = true;
    const frame = this.#chunks.length === 1
      ? this.#chunks[0]!
      : Buffer.concat(this.#chunks, this.#totalBytes);
    return { complete: true, payload: frame.subarray(4) };
  }

  get complete(): boolean {
    return this.#complete;
  }
}
