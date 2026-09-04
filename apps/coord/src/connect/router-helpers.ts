// Shared, pure Connect-handler helpers — no router/module state. Importable
// by router.ts AND the split connect/handlers-*.ts domain files without a
// runtime import cycle (router.ts ↔ handlers-* would otherwise both import
// each other's values). Add a helper here when >1 handler file needs it.

import { randomUUID } from "node:crypto";
import { Code, ConnectError } from "@connectrpc/connect";
import { log } from "@roost/shared/log";
import type { ClientControlFrame } from "@roost/shared/wire";
import type { KyselyDB } from "../db/connection.ts";
import type { AccountDeviceCaller, DashboardActor } from "./auth-interceptor.ts";
import { getWorkerHubSocket } from "./worker-service.ts";

export type WorkerHubSocket = { send(data: string | Uint8Array): void };

/** A session row bound to its live worker hub socket. */
export interface SessionWorkerBinding {
  row: { worker_fp: string };
  sock: WorkerHubSocket;
}

// Load a session's worker and its live hub socket in one step. Every
// forwarding handler repeated this preamble with DRIFTED error strings
// ("unknown session" vs "session not found"); these two wordings are THE
// contract — do not re-diverge them.
export async function requireSessionWorkerSocket(
  db: KyselyDB,
  actor: DashboardActor,
  sessionId: string,
): Promise<SessionWorkerBinding> {
  const row = await db.selectFrom("sessions as session")
    .innerJoin("workers as worker", "worker.fp", "session.worker_fp")
    .select("session.worker_fp as worker_fp")
    .where("session.id", "=", sessionId)
    .where("session.dashboard_id", "=", actor.dashboardId)
    .where("worker.dashboard_id", "=", actor.dashboardId)
    .where("worker.deleted_at_ms", "is", null)
    .executeTakeFirst();
  if (!row) throw new ConnectError("session not found", Code.NotFound);
  const sock = getWorkerHubSocket(row.worker_fp);
  if (!sock) throw new ConnectError("worker offline", Code.Unavailable);
  return { row, sock };
}



// Wire-boundary validation for proto3 required-string / required-array
// fields. Proto3 has no length facet so an unset field arrives as "" or [].
// Used by mcpCreate.
export function requireNonEmpty(value: string | unknown[], field: string): void {
  if (value.length === 0) {
    throw new ConnectError(`${field} is required`, Code.InvalidArgument);
  }
}

// Single source of truth for the browser-command JSON envelope. Used by
// every Connect handler that forwards a request through the worker hub.
// Throws ConnectError on send failure so handlers can `await pending.promise`
// immediately after.
export function sendBrowserCmd(
  sock: WorkerHubSocket,
  caller: AccountDeviceCaller,
  requestId: string,
  frame: ClientControlFrame,
): void {
  const downstream = {
    kind: "browser-command" as const,
    browser_id: caller.fingerprint,
    // Legacy JSON worker commands retain sender attribution only; terminal
    // input has its own proof-carrying transport and terminal views never pass
    // through this envelope.
    viewer_id: caller.fingerprint,
    request_id: requestId,
    frame,
  };
  try { sock.send(JSON.stringify(downstream)); }
  catch (e) {
    throw new ConnectError(`send failed: ${String(e)}`, Code.Unavailable);
  }
}

// Resolve a session's worker and forward a control frame, fire-and-ack.
// Returns false (never throws) when the session/worker is gone so handlers can
// report accepted:false.
export async function forwardToSessionWorker(
  db: KyselyDB,
  actor: DashboardActor,
  sessionIdRaw: string,
  caller: AccountDeviceCaller,
  frame: ClientControlFrame,
): Promise<boolean> {
  // Frames that carry their own request_id reuse it as the envelope id so
  // worker logs and RPC replies share one identifier; all others get a UUID.
  const requestId = "request_id" in frame ? frame.request_id : randomUUID();
  let binding: SessionWorkerBinding;
  try {
    binding = await requireSessionWorkerSocket(db, actor, sessionIdRaw);
  } catch {
    return false;
  }
  try { sendBrowserCmd(binding.sock, caller, requestId, frame); return true; }
  catch (e) { log.warn("router-helpers.forwardToSessionWorker", "send_failed", { error: String(e) }); return false; }
}
