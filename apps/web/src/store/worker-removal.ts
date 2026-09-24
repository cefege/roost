// Applies the durable worker-removal boundary to browser replica and direct state.
// Saved sessions/workspaces remain offline history; browser-held credentials and
// carriers retire before the machine record disappears.
import type { Worker } from "@roost/protocol/wire";
import { terminalDirectRegistry } from "./terminal-stream-transport.ts";
import { retireTerminalGrantsForWorker } from "../ws/local-terminal-grants.ts";
import { deleteStoreRecord } from "./root.ts";
import { workerOnline } from "./sync-routable.ts";

/** Apply only a coordinator-confirmed worker revocation to the local replica.
 * Sessions and workspaces deliberately remain as offline history. */
export function applyWorkerDeleteResponse(
  fp: string,
  response: { ok: boolean },
): boolean {
  if (response.ok !== true) return false;
  applyWorkerRemoval(fp);
  return true;
}

export function applyWorkerRemoval(fp: string): void {
  retireTerminalGrantsForWorker(fp);
  terminalDirectRegistry.retireWorker(fp, "worker removed");
  deleteStoreRecord("workers", fp);
}

/** A missing worker is a permanent-offboarding breadcrumb and therefore just
 * as offline as a registered worker without a route. */
export function sessionWorkerIsOffline(worker: Worker | undefined): boolean {
  return worker === undefined || !workerOnline(worker);
}
