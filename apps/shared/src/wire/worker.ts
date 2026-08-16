// Worker registry shape. Coord persists; SPA reads via tRPC.
// REWRITE.md R3 data model.

import { z } from "zod";
import { WorkerFp } from "./brand.ts";

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
  // Non-null = this worker's keeper subprocess is running stale code (value =
  // the running keeper's short build stamp); null = current. Drives the
  // MachinesPane "keeper stale" badge + `roost keeper-refresh`. null for workers
  // reporting before the field existed.
  keeper_stale: z.string().nullable(),
});
export type Worker = z.infer<typeof Worker>;

// Worker presence delta — SSE stream payload.
export const WorkerPresenceEvent = z.discriminatedUnion("kind", [
  z.object({ kind: z.literal("registered"), worker: Worker }),
  z.object({ kind: z.literal("heartbeat"), fp: WorkerFp, last_seen_ms: z.number().int(), host_metrics: HostMetrics.nullable() }),
  z.object({ kind: z.literal("removed"), fp: WorkerFp }),
]);
export type WorkerPresenceEvent = z.infer<typeof WorkerPresenceEvent>;
