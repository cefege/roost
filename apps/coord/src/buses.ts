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

/** Internal-only ownership stamp consumed by raw Sync fan-out. It is never
 * serialized into a wire payload; producers derive it from the persisted
 * actor/resource row before publishing. */
export interface DashboardScoped {
  _dashboard_id?: string;
}

export type DashboardScopedMessage<T> = T & DashboardScoped;
/** Durable session fan-out always carries the scope committed with its event
 * row. `_event_id` is an internal replay-order stamp, never a wire field. */
export type SessionBusMessage = DashboardScopedMessage<SessionEvent> & {
  readonly _dashboard_id: string;
  readonly _event_id?: number;
};


// taskBus carries proto-typed Task deltas directly — the firehose
// taskFrame builder is now a thin wrapper, and the per-mutation
// publishTaskState() in router.ts feeds taskRowToProto(row) without the
// JSON.parse/stringify round-trip the old Zod-shape relay caused.
export type TaskBusMsg = DashboardScopedMessage<{
  kind: "created" | "state";
  task: PbTask;
}>;
// pairBus carries pending-pair-request deltas (perf sweep C2.4 — replaces the
// SPA's 5 s pairList poller). Coord-internal shape (no cross-boundary
// validation, so no shared wire schema): published by the pair
// handlers (create/approve/deny), consumed only by the firehose pairFrame
// adapter. `pending` upserts; `removed` drops by ephemeral_id.
export type PairRequestDelta = DashboardScopedMessage<
  | { kind: "pending"; ephemeral_id: string; label: string; created_at_ms: number }
  | { kind: "removed"; ephemeral_id: string }
>;
// AuditRow inline type (router/audit.ts deleted in crpc6).
export interface AuditRow extends DashboardScoped {
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
  private readonly listeners = new Map<Listener<T>, string | undefined>();
  private readonly ring: T[] = [];
  private readonly capacity: number;

  constructor(capacity = 64) {
    this.capacity = capacity;
  }

  publish(msg: T): void {
    this.ring.push(msg);
    if (this.ring.length > this.capacity) this.ring.shift();
    const scoped = msg as T & DashboardScoped;
    for (const [fn, dashboardId] of this.listeners) {
      if (
        dashboardId !== undefined
        && scoped._dashboard_id !== dashboardId
      ) continue;
      try { fn(msg); } catch (e) { diag("bus.listener_throw", { error: String(e) }); /* listener errors must not kill publisher */ }
    }
  }

  // subscribe returns an unsubscribe function. Does NOT replay the ring —
  // reconnect backfill goes through the events table, not bus history.
  subscribe(fn: Listener<T>, dashboardId?: string): () => void {
    this.listeners.set(fn, dashboardId);
    return () => { this.listeners.delete(fn); };
  }

  // Live listener count — ≈ open Sync streams for the firehose-coupled buses.
  // uiDispatch reports it as `delivered` so a headless caller can tell
  // "command published into the void" (0) from "some browser will act" (>0).
  get subscriberCount(): number { return this.listeners.size; }

  subscriberCountFor(dashboardId: string): number {
    let count = 0;
    for (const scope of this.listeners.values()) {
      if (scope === dashboardId) count += 1;
    }
    return count;
  }
}

// ─── singletons ────────────────────────────────────────────────────────

export const presenceBus    = new BoundedBus<DashboardScopedMessage<WorkerPresenceEvent>>(128);
export const sessionBus     = new BoundedBus<SessionBusMessage>(256);
export const workspaceBus   = new BoundedBus<DashboardScopedMessage<WorkspaceDelta>>(64);
export const taskBus        = new BoundedBus<TaskBusMsg>(64);
export const mcpBus         = new BoundedBus<DashboardScopedMessage<McpStreamMessage>>(128);
// audit_log row inserts — ring size 256 so short-lived SSE subscribers don't
// miss bursts during reconnect.
export const auditBus       = new BoundedBus<AuditRow>(256);
// pending pair requests — low traffic (a handful per pairing ceremony).
export const pairBus        = new BoundedBus<PairRequestDelta>(32);


// OSC 0/2 terminal title, parsed CENTRALLY by coord (terminal-title-hub) from
// the same relayed byte stream and fanned out via Sync. VOLATILE —
// one value per session, published only on CHANGE, seeded to
// fresh Sync subscribers. The title is coord-authoritative: it is parsed once
// here, never per-browser, so every viewer of a session agrees on it.
export const titleBus = new BoundedBus<DashboardScopedMessage<{
  session_id: string;
  title: string;
}>>(256);

// Last-activity timestamp (ms) per session, stamped by coord (last-activity-hub)
// on PTY byte flow and fanned out via Sync (throttled, not per-byte). VOLATILE
// like the two above — seeded to fresh Sync subscribers, throttled live updates
// after. Drives the sidebar "Last activity" filter aging out idle OPEN sessions.
export const lastActivityBus = new BoundedBus<DashboardScopedMessage<{
  session_id: string;
  ts_ms: number;
}>>(256);

// Worker routability = coord's live raw-WS membership (connectWorkers). The
// AUTHORITATIVE "server is reachable right now" signal — distinct from
// last_seen_ms heartbeat freshness. Published as the FULL current set on
// every connect/disconnect (cheap: a handful of workers) + seeded on each
// Sync connect, so the SPA's online indicator updates live instead of only
// on the periodic workersList snapshot (the "active server shows red" bug).
export const workerRoutableBus = new BoundedBus<DashboardScopedMessage<{ fps: string[] }>>(64);

// phase-26 firehose: session_id-tagged global fanouts for PTY bytes
// and presence frames. publishBytes / publishPresence dual-publish to
// these so the firehose subscription can deliver all session bytes +
// presence in a single SSE stream. Eliminates the N-subs-per-Terminal
// pressure on Chrome's 6-per-origin HTTP/1.1 connection budget.
export const globalBytesBus = new BoundedBus<DashboardScopedMessage<{
  session_id: string;
  bytes: Uint8Array;
}>>(512);
export const globalPresenceBus = new BoundedBus<DashboardScopedMessage<{
  session_id: string;
  data: unknown;
}>>(64);

// Volatile coding-agent state. Active updates upsert a session; inactive
// updates delete it. The coordinator hub owns revision ordering and seeds
// current active values to every fresh Sync connection.
export const agentStatusBus = new BoundedBus<DashboardScopedMessage<AgentStatusUpdate>>(128);



// ui-cc — browser-tab UI state + command relay (G1/G2). VOLATILE,
// presence-class, no replay: layout stays browser-local; coord only relays.
// `state` msgs re-broadcast a tab's UiReportState (also kept in the
// handlers-ui map, re-seeded to fresh Sync subscribers from there);
// `command` msgs are fire-and-forget UiDispatch payloads the live SPA
// tab(s) execute with their existing pure layout ops — never seeded.
export type UiBusMsg = DashboardScopedMessage<
  | { kind: "state"; fp: string; tabId: string; state: UiReportStateRequest }
  | { kind: "command"; targetTabId: string; command: UiCommand }
>;
export const uiBus = new BoundedBus<UiBusMsg>(64);
