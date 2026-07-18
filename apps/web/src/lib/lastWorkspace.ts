// Persist the last workspace the user interacted with, per worker, to
// localStorage. Workspaces themselves live in coord SQLite and rehydrate
// via workspacesList on boot — this just remembers WHICH workspace to
// scroll-into-view / expand on refresh so the user doesn't have to
// re-pick a folder after every page reload.
//
// Key shape: roost.lastWorkspaceId.<workerFp> = <workspaceId>
// Written by SessionRow on click + by spawn handlers.

import type { WorkerFp, WorkspaceId } from "@roost/shared/wire";

const KEY_PREFIX = "roost.lastWorkspaceId.";

export function rememberLastWorkspace(workerFp: WorkerFp, workspaceId: WorkspaceId): void {
  try { localStorage.setItem(KEY_PREFIX + workerFp, workspaceId); } catch { /* quota / privacy mode */ }
}
