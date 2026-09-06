// Authenticated coordinator routability is the live worker proof used by
// rollout admission and convergence. SQLite heartbeats describe recent state
// but cannot prove that the coordinator can currently reach a worker.

import { buildDashboardScopedCliContext } from "./cli-auth.ts";

export async function routableWorkerFingerprints(): Promise<ReadonlySet<string>> {
  const { client } = await buildDashboardScopedCliContext();
  const response = await client.workersList({});
  return new Set(response.routableFps);
}
