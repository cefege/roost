// Applies the durable worker-removal boundary to browser replica state.
// Only the machine record disappears; saved sessions/workspaces remain as
// offline history and missing workers stay force-removable.
import type { Worker } from "@roost/shared/wire";
import { deleteStoreRecord } from "./root.ts";
import { workerOnline } from "./sync-routable.ts";

/** Apply only a coordinator-confirmed worker revocation to the local replica.
 * Sessions and workspaces deliberately remain as offline history. */
export function applyWorkerDeleteResponse(
  fp: string,
  response: { ok: boolean },
): boolean {
  if (response.ok !== true) return false;
  deleteStoreRecord("workers", fp);
  return true;
}

/** A missing worker is a permanent-offboarding breadcrumb and therefore just
 * as offline as a registered worker without a route. */
export function sessionWorkerIsOffline(worker: Worker | undefined): boolean {
  return worker === undefined || !workerOnline(worker);
}
