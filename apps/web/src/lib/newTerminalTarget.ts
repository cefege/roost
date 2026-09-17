// Resolves which machine a new terminal should open on.
// SidebarNewTerminal's action bar and HomeLanding's compact affordance both read
// it, so the online filter and the active-route preference stay one value.
// Reads the reactive worker/session stores: call inside a memo or JSX.

import type { Worker } from "@roost/shared/wire";
import { rootStore } from "../store/root.ts";
import { activeSessionForPath, allSessions } from "../store/selectors.ts";
import { workerOnline } from "../store/sync.ts";

/** Online workers sorted by label — the machine-menu order. */
export function onlineWorkersByLabel(): Worker[] {
  return Object.values(rootStore.workers)
    .filter(workerOnline)
    .sort((left, right) => left.label.localeCompare(right.label));
}

/** URL-active worker → newest session's online worker → first online worker → null. */
export function defaultNewTerminalWorkerFp(pathname: string): string | null {
  const workers = onlineWorkersByLabel();
  const activeWorkerFp = activeSessionForPath(pathname)?.worker_fp;
  if (activeWorkerFp && workers.some((worker) => worker.fp === activeWorkerFp)) return activeWorkerFp;
  const recentWorkerFp = [...allSessions()]
    .sort((left, right) => right.created_at - left.created_at)
    .find((session) => workers.some((worker) => worker.fp === session.worker_fp))?.worker_fp;
  return recentWorkerFp ?? workers[0]?.fp ?? null;
}
