// Per-domain delta handlers — fold one decoded wire event into rootStore.
// Split out of store/sync.ts (400-line cap). The firehose (_runConnectSync in
// sync.ts) decodes each Sync-stream frame to its wire shape then calls the
// matching _handle* here; this module imports root/projector/dispatch only,
// never sync.ts → no import cycle. Also owns the keeper-death respawn-toast
// detector + the webhook/audit delta subscriber registries (iterated by the
// firehose, which imports the const Sets).

import { deleteStoreRecord, setRootStore } from "./root.ts";
import { foldEventIntoStore } from "./projector.ts";
import type { Worker, Workspace, Task, PermissionRule, McpRelay } from "@roost/shared/wire";

// Keeper-death awareness toast. A burst of `respawned` events while the SPA
// stream is steady AND no worker just re-registered = a keeper died mid-life
// (jetsam/OOM kill of the keeper subprocess) and the worker auto-recovered the
// sessions as fresh PTYs. Surfaced as ONE coalesced warn toast so the user
// knows terminals lost history + can weigh adding RAM. Suppressed during SPA
// (re)connect backfill (_lastSyncConnectAt) and worker-restart/deploy resume
// (_lastWorkerRegisterAt) — both legitimately emit `respawned` bursts that are
// NOT memory-pressure events. See memory project_keeper_death_auto_respawn.
const RESPAWN_TOAST_GRACE_MS = 10_000;
let _lastSyncConnectAt = 0;
let _lastWorkerRegisterAt = 0;
let _keeperDeathRespawnCount = 0;
let _keeperDeathToastTimer: ReturnType<typeof setTimeout> | null = null;

/** Mark a sync-stream (re)connect — suppresses respawn-toasts for backfill. */
export function _noteSyncConnect(): void { _lastSyncConnectAt = performance.now(); }

function _maybeToastKeeperDeath(): void {
  const now = performance.now();
  if (now - _lastSyncConnectAt < RESPAWN_TOAST_GRACE_MS) return;   // SPA (re)connect backfill
  if (now - _lastWorkerRegisterAt < RESPAWN_TOAST_GRACE_MS) return; // worker restart / deploy
  _keeperDeathRespawnCount++;
  if (_keeperDeathToastTimer) return; // coalesce the burst into one toast
  _keeperDeathToastTimer = setTimeout(() => {
    const n = _keeperDeathRespawnCount;
    _keeperDeathRespawnCount = 0;
    _keeperDeathToastTimer = null;
    void import("./toastStore.ts").then(({ addToast }) =>
      addToast(
        `${n} terminal${n === 1 ? "" : "s"} restarted — the server ran low on memory and recovered ${n === 1 ? "it" : "them"}. Scrollback before this point was lost. If this keeps happening, free up RAM or add memory.`,
        "warn",
      ));
  }, 1500);
}

export function _handleSessionsEvent(event: unknown): void {
  type SessionEventLike = Parameters<typeof foldEventIntoStore>[0];
  foldEventIntoStore(event as SessionEventLike);
  if (event && typeof event === "object" && "kind" in event && event.kind === "respawned") {
    _maybeToastKeeperDeath();
  }
}
export function _handlePresenceEvent(event: unknown): void {
  const ev = event as
    | { kind: "registered"; worker: Worker }
    | { kind: "heartbeat"; fp: string; last_seen_ms: number; host_metrics: unknown }
    | { kind: "removed"; fp: string };
  if (ev.kind === "registered") {
    // Timestamp the (re)register so a respawn burst within the grace window is
    // attributed to a worker restart/deploy, not a keeper-death memory event.
    _lastWorkerRegisterAt = performance.now();
    setRootStore("workers", ev.worker.fp, ev.worker);
  } else if (ev.kind === "heartbeat") {
    setRootStore("workers", ev.fp, (prev) =>
      prev
        ? { ...prev, last_seen_ms: ev.last_seen_ms, host_metrics: ev.host_metrics as Worker["host_metrics"] }
        : prev,
    );
  } else if (ev.kind === "removed") {
    deleteStoreRecord("workers", ev.fp);
  }
}
export function _handleWorkspacesDelta(event: unknown): void {
  const d = event as
    | { kind: "created" | "updated"; workspace: Workspace }
    | { kind: "deleted"; id: string }
    | { kind: "sessions-set"; id: string; session_ids: string[]; version: number };
  if (d.kind === "created" || d.kind === "updated") {
    setRootStore("workspaces", d.workspace.id, d.workspace);
  } else if (d.kind === "deleted") {
    deleteStoreRecord("workspaces", d.id);
  } else if (d.kind === "sessions-set") {
    setRootStore("workspaces", d.id, (prev) =>
      prev ? { ...prev, session_ids: d.session_ids as Workspace["session_ids"], version: d.version } : prev,
    );
  }
}
export function _handleTasksDelta(event: unknown): void {
  const d = event as { task: Task };
  setRootStore("tasks", d.task.id, d.task);
}
export function _handlePermissionsDelta(event: unknown): void {
  const d = event as
    | { kind: "created" | "updated"; rule: PermissionRule }
    | { kind: "deleted"; id: string };
  if (d.kind === "created" || d.kind === "updated") {
    setRootStore("permission_rules", d.rule.id, d.rule);
  } else if (d.kind === "deleted") {
    deleteStoreRecord("permission_rules", d.id);
  }
}
export function _handleMcpEvent(event: unknown): void {
  const msg = event as
    | { kind: "created" | "updated"; relay: McpRelay }
    | { kind: "deleted"; id: string }
    | { relay_id: string };
  if ("kind" in msg && (msg.kind === "created" || msg.kind === "updated")) {
    setRootStore("mcp_relays", msg.relay.id, msg.relay);
  } else if ("kind" in msg && msg.kind === "deleted") {
    deleteStoreRecord("mcp_relays", msg.id);
  }
}

// crpc6 — per-bus delta dispatchers (webhooks + audit ride the firehose).
// Panes subscribe via registerWebhookDelta / registerAuditDelta; unsubscribe
// on component cleanup.
export const _webhookDeltaSubs = new Set<(d: unknown) => void>();
export const _auditDeltaSubs = new Set<(d: unknown) => void>();
export function registerWebhookDelta(fn: (d: unknown) => void): () => void {
  _webhookDeltaSubs.add(fn);
  return () => _webhookDeltaSubs.delete(fn);
}
export function registerAuditDelta(fn: (d: unknown) => void): () => void {
  _auditDeltaSubs.add(fn);
  return () => _auditDeltaSubs.delete(fn);
}
