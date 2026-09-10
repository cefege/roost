// BoundedBus<T>: in-process pub/sub with a fixed-size replay ring.
// One singleton per domain; subscribers get a cleanup function.
// Ring capacity: last N events replayed on subscribe (default 64).
// R1.1 broadcast pattern ported from legacy lib/broadcast.ts.

import type { SessionEvent } from "@roost/shared/wire";
import { diag } from "@roost/shared/diag";
import type { WorkerPresenceEvent } from "@roost/shared/wire";
import type { WorkspaceDelta } from "@roost/shared/wire";
import type { McpStreamMessage } from "@roost/shared/wire";
import type { AgentStatusUpdate } from "@roost/shared/wire";
import type { Task as PbTask } from "@roost/shared/proto/wire_pb";
import type { UiReportStateRequest, UiCommand } from "@roost/shared/proto/sync_pb";

/** Durable session fan-out carries an internal replay-order stamp only;
 * `_event_id` is never a wire field. */
export type SessionBusMessage = SessionEvent & {
  readonly _event_id?: number;
};


// taskBus carries proto-typed Task deltas directly — the firehose
// taskFrame builder is now a thin wrapper, and the per-mutation
// publishTaskState() in router.ts feeds taskRowToProto(row) without the
// JSON.parse/stringify round-trip the old Zod-shape relay caused.
export type TaskBusMsg = {
  kind: "created" | "state";
  task: PbTask;
};
// pairBus carries pending-pair-request deltas (perf sweep C2.4 — replaces the
// SPA's 5 s pairList poller). Coord-internal shape (no cross-boundary
// validation, so no shared wire schema): published by the pair
// handlers (create/approve/deny), consumed only by the firehose pairFrame
// adapter. `pending` upserts; `removed` drops by ephemeral_id.
export type PairRequestDelta =
  | {
      kind: "pending";
      ephemeral_id: string;
      label: string;
      created_at_ms: number;
      user_agent: string;
      client_browser: string;
      client_os: string;
      client_device_type: string;
      source_ip: string;
      country_code: string;
      region: string;
      city: string;
      edge_identity_provider: string;
      edge_identity: string;
      edge_identity_verified: boolean;
      expires_at_ms: number;
    }
  | { kind: "removed"; ephemeral_id: string };
// AuditRow inline type (router/audit.ts deleted in crpc6).
export interface AuditRow {
  id: number;
  ts: number;
  caller_fp: string | null;
  caller_label: string | null;
  method: string;
  path: string;
  status: number;
  trace_id: string | null;
}

type Listener<T> = (msg: T) => void;

export class BoundedBus<T> {
  private readonly listeners = new Set<Listener<T>>();
  private readonly ring: T[] = [];
  private readonly capacity: number;

  constructor(capacity = 64) {
    this.capacity = capacity;
  }

  publish(msg: T): void {
    this.ring.push(msg);
    if (this.ring.length > this.capacity) this.ring.shift();
    for (const fn of this.listeners) {
      try { fn(msg); } catch (e) { diag("bus.listener_throw", { error: String(e) }); /* listener errors must not kill publisher */ }
    }
  }

  // subscribe returns an unsubscribe function. Does NOT replay the ring —
  // reconnect backfill goes through the events table, not bus history.
  subscribe(fn: Listener<T>): () => void {
    this.listeners.add(fn);
    return () => { this.listeners.delete(fn); };
  }

  // Live listener count — ≈ open Sync streams for the firehose-coupled buses.
  // uiDispatch reports it as `delivered` so a headless caller can tell
  // "command published into the void" (0) from "some browser will act" (>0).
  get subscriberCount(): number { return this.listeners.size; }
  /** Diagnostics/test seam for owners whose replay retention must be zero. */
  get retainedCount(): number { return this.ring.length; }
}

// ─── singletons ────────────────────────────────────────────────────────

export const presenceBus    = new BoundedBus<WorkerPresenceEvent>(128);
export const sessionBus     = new BoundedBus<SessionBusMessage>(256);
export const workspaceBus   = new BoundedBus<WorkspaceDelta>(64);
export const taskBus        = new BoundedBus<TaskBusMsg>(64);
export const mcpBus         = new BoundedBus<McpStreamMessage>(128);
// audit_log row inserts — ring size 256 so short-lived SSE subscribers don't
// miss bursts during reconnect.
export const auditBus       = new BoundedBus<AuditRow>(256);
// pending pair requests — low traffic (a handful per pairing ceremony).
export const pairBus        = new BoundedBus<PairRequestDelta>(32);


// OSC 0/2 terminal title, supplied as semantic worker metadata and fanned out
// via Sync. One value per session is published only on meaningful change and
// seeded to fresh Sync subscribers; browser clients never parse PTY bytes.
export const titleBus = new BoundedBus<{
  session_id: string;
  title: string;
}>(256);

// Last-activity timestamp (ms) from semantic worker observations. The
// coordinator throttles live fan-out while retained snapshots let a fresh
// subscriber age idle OPEN sessions immediately.
export const lastActivityBus = new BoundedBus<{
  session_id: string;
  ts_ms: number;
}>(256);

// Worker routability = coord's live raw-WS membership (connectWorkers). The
// AUTHORITATIVE "server is reachable right now" signal — distinct from
// last_seen_ms heartbeat freshness. Published as the FULL current set on
// every connect/disconnect (cheap: a handful of workers) + seeded on each
// Sync connect, so the SPA's online indicator updates live instead of only
// on the periodic workersList snapshot (the "active server shows red" bug).
export const workerRoutableBus = new BoundedBus<{ fps: string[] }>(64);

// Global worker-presence fan-out. Presence frames share one stream so a
// browser need not subscribe independently for each monitored worker.
export const globalPresenceBus = new BoundedBus<{
  session_id: string;
  data: unknown;
}>(64);

// Volatile coding-agent state. Active updates upsert a session; inactive
// updates delete it. The coordinator hub owns revision ordering and seeds
// current active values to every fresh Sync connection.
export const agentStatusBus = new BoundedBus<AgentStatusUpdate>(128);



// UI command control is volatile and never replayed. State messages are kept
// separately for fresh Sync seeds; the first eight legacy commands retain
// publication-count delivery, while acknowledged apply is socket-generation
// fenced and filtered by each v2 feed before it reaches a browser.
export type UiBusMsg =
  | { kind: "state"; fp: string; tabId: string; state: UiReportStateRequest }
  | { kind: "command"; targetTabId: string; command: UiCommand }
  | {
      kind: "apply";
      targetTabId: string;
      targetSocketId: string;
      correlationId: string;
      command: UiCommand;
    };
export const uiBus = new BoundedBus<UiBusMsg>(0);
