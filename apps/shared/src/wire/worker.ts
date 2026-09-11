// Worker registry shape. Coord persists; the SPA reads through coordinator RPCs.

import { z } from "zod";
import { WorkerFp } from "./brand.ts";
import { KeeperRuntimeObservationV1Schema } from "../keeper-update.ts";
import { TerminalCoreCapacityReportSchema } from "../terminal-core-capacity.ts";
export type { TerminalCoreCapacityReport } from "../terminal-core-capacity.ts";

export const HOST_IDENTITY_VALUE_MAX_UTF8_BYTES = 256;
const HOST_IDENTITY_MAX_INSPECTED_CODE_UNITS = 4_096;
function utf8CodePointBytes(codePoint: number): number {
  return codePoint <= 0x7f
    ? 1
    : codePoint <= 0x7ff
      ? 2
      : codePoint <= 0xffff
        ? 3
        : 4;
}


export const HostIdentity = z.object({
  hardware_model: z.string().nullable(),
  chip: z.string().nullable(),
  linux_distribution: z.string().nullable(),
});
export type HostIdentity = z.infer<typeof HostIdentity>;

/** Produces a bounded, single-line display value without terminal or bidi controls. */
export function normalizeHostIdentityText(value: unknown): string | null {
  if (typeof value !== "string") return null;

  let inspected = 0;
  let encodedBytes = 0;
  let output = "";
  for (let index = 0; index < value.length && inspected < HOST_IDENTITY_MAX_INSPECTED_CODE_UNITS;) {
    const codePoint = value.codePointAt(index);
    if (codePoint === undefined) break;
    const codeUnits = codePoint > 0xffff ? 2 : 1;
    index += codeUnits;
    inspected += codeUnits;

    let character = String.fromCodePoint(codePoint);
    if (/\s/u.test(character)) {
      character = " ";
    } else if (
      /[\p{Cc}\p{Cf}]/u.test(character)
      || (codePoint >= 0xd800 && codePoint <= 0xdfff)
    ) {
      continue;
    }

    const characterBytes = utf8CodePointBytes(character.codePointAt(0)!);
    if (encodedBytes + characterBytes > HOST_IDENTITY_VALUE_MAX_UTF8_BYTES) break;
    encodedBytes += characterBytes;
    output += character;
  }

  const normalized = output.normalize("NFC").replace(/ +/g, " ").trim();
  encodedBytes = 0;
  output = "";
  for (const character of normalized) {
    const characterBytes = utf8CodePointBytes(character.codePointAt(0)!);
    if (encodedBytes + characterBytes > HOST_IDENTITY_VALUE_MAX_UTF8_BYTES) {
      break;
    }
    encodedBytes += characterBytes;
    output += character;
  }
  return output || null;
}

/** Canonicalizes all static host fields so database and browser values stay safe. */
export function normalizeHostIdentity(value: unknown): HostIdentity | null {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return null;
  }
  const fields = value as Record<string, unknown>;
  const identity = {
    hardware_model: normalizeHostIdentityText(fields.hardware_model),
    chip: normalizeHostIdentityText(fields.chip),
    linux_distribution: normalizeHostIdentityText(fields.linux_distribution),
  };
  return identity.hardware_model || identity.chip || identity.linux_distribution
    ? identity
    : null;
}

export const HostMetrics = z.object({
  cpu_pct: z.number().min(0).max(100),
  mem_used_bytes: z.number().int().nonnegative(),
  mem_total_bytes: z.number().int().nonnegative(),
  disk_used_bytes: z.number().int().nonnegative(),
  disk_total_bytes: z.number().int().nonnegative(),
  net_rx_bps: z.number().int().nonnegative(),
  net_tx_bps: z.number().int().nonnegative(),
  sampled_at_ms: z.number().int().positive(),
});
export type HostMetrics = z.infer<typeof HostMetrics>;

export const Worker = z.object({
  fp: WorkerFp,
  label: z.string().min(1),
  os: z.enum(["darwin", "linux", "win32"]),
  host_identity: HostIdentity.nullable(),
  git_sha: z.string().nullable(),            // drift badge; null = pre-0018
  host_metrics: HostMetrics.nullable(),      // volatile; decays on disconnect
  registered_at_ms: z.number().int().positive(),
  last_seen_ms: z.number().int().positive(),
  // Re-added in migration 0005 for the SPA right-click "Screen Share" /
  // "SSH" menu. Stored from the worker's ROOST_REACHABLE_ADDR env at
  // register time; not used to dial the worker (worker has no inbound
  // surface — it dials coord). Nullable for workers registered before
  // the field was added.
  reachable_addr: z.string().nullable(),
  keeper_runtime: KeeperRuntimeObservationV1Schema.nullable(),
  terminal_core_capacity: TerminalCoreCapacityReportSchema.nullable(),
});
export type Worker = z.infer<typeof Worker>;

// Worker Sync presence; static fields travel only in the full registered record.
export const WorkerPresenceEvent = z.discriminatedUnion("kind", [
  z.object({ kind: z.literal("registered"), worker: Worker }),
  z.object({
    kind: z.literal("heartbeat"),
    fp: WorkerFp,
    last_seen_ms: z.number().int(),
    host_metrics: HostMetrics.nullable(),
    terminal_core_capacity: TerminalCoreCapacityReportSchema.nullable(),
  }),
  z.object({ kind: z.literal("removed"), fp: WorkerFp }),
]);
export type WorkerPresenceEvent = z.infer<typeof WorkerPresenceEvent>;
