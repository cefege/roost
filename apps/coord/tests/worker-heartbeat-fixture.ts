// One mutable workers row, the update patches the heartbeat handler applies to
// it, and the authenticated worker caller context that handler requires.
// Shared by the worker-heartbeat-* suites so the row shape cannot drift between
// the platform self-heal and keeper-runtime proofs.

import type { HandlerContext } from "@connectrpc/connect";
import { callerKey } from "../src/connect/auth-interceptor.ts";
import { makeWorkerHandlers } from "../src/connect/handlers-workers.ts";
import type { ConnectDeps } from "../src/connect/router.ts";

export const WORKER_FP = "c".repeat(64);
export const WORKER_LABEL = "mac old";

export type WorkerRow = Record<string, unknown>;

export interface WorkerHeartbeatDb {
  db: ConnectDeps["db"];
  row: () => WorkerRow;
  patches: WorkerRow[];
}

export function workerHeartbeatDb(
  dashboardId: string,
  overrides: WorkerRow = {},
): WorkerHeartbeatDb {
  let row: WorkerRow = {
    fp: WORKER_FP,
    dashboard_id: dashboardId,
    label: WORKER_LABEL,
    os: "linux",
    git_sha: null,
    host_metrics_json: null,
    registered_at_ms: 1,
    last_seen_ms: 1,
    reachable_addr: null,
    keeper_runtime_json: null,
    ...overrides,
  };
  const patches: WorkerRow[] = [];
  const selectQuery = {
    select: () => selectQuery,
    selectAll: () => selectQuery,
    where: () => selectQuery,
    executeTakeFirst: async () => row,
  };
  return {
    db: {
      selectFrom: () => selectQuery,
      updateTable: () => {
        let patch: WorkerRow = {};
        const apply = async () => {
          row = { ...row, ...patch };
          patches.push(patch);
          return row;
        };
        const updateQuery = {
          set: (value: WorkerRow) => {
            patch = value;
            return updateQuery;
          },
          where: () => updateQuery,
          returningAll: () => updateQuery,
          executeTakeFirst: apply,
          executeTakeFirstOrThrow: apply,
        };
        return updateQuery;
      },
    } as unknown as ConnectDeps["db"],
    row: () => row,
    patches,
  };
}

export function workerHeartbeatContext(): HandlerContext {
  const worker = {
    kind: "worker" as const,
    fingerprint: WORKER_FP,
    label: WORKER_LABEL,
  };
  return {
    values: { get: (key: unknown) => key === callerKey ? worker : undefined },
  } as unknown as HandlerContext;
}

export function workerHeartbeatHandlers(db: ConnectDeps["db"]) {
  return makeWorkerHandlers({
    db,
    cfg: {},
  } as unknown as ConnectDeps);
}
