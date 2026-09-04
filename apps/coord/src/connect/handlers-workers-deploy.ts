// Owns worker deployment RPCs plus their output stream.
// The scope map is created with each handler set so one dashboard cannot read
// another dashboard's job, while its delayed eviction outlives every supported
// Unix or Windows deployment timeout.

import { create } from "@bufbuild/protobuf";
import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import {
  type CoordinatorService,
  WorkersDeployOutputFrameSchema,
  WorkersDeployStartResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { DEPLOY_JOB_TTL_MS, deployOutput, startDeploy } from "../deploy-jobs.ts";
import { SseQueueOverflowError } from "../sse.ts";
import { startWindowsDeploy } from "../windows-update-manifest.ts";
import { WINDOWS_UPDATE_TIMEOUT_MS } from "../windows-update-deploy-record.ts";
import { requireDashboardAdmin } from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";

const DEPLOY_JOB_SCOPE_TTL_MS = DEPLOY_JOB_TTL_MS + WINDOWS_UPDATE_TIMEOUT_MS;

type WorkerDeployMethods =
  | "workersDeployOutput"
  | "workersDeployStart";

export type WorkerDeployRecord = {
  fp: string;
  os: string | null;
  label: string;
  reachable_addr: string | null;
};

export type WorkerDeployTargetResolution =
  | { worker: WorkerDeployRecord; error: null }
  | { worker: undefined; error: null }
  | { worker: undefined; error: string };

export function resolveWorkerDeployTarget(
  workers: readonly WorkerDeployRecord[],
  requestedHost: string,
): WorkerDeployTargetResolution {
  const fingerprintMatches = workers.filter((worker) => worker.fp === requestedHost);
  if (fingerprintMatches.length > 1) {
    return {
      worker: undefined,
      error: `ambiguous deploy target "${requestedHost}" matches multiple worker fingerprints`,
    };
  }
  if (fingerprintMatches[0]) return { worker: fingerprintMatches[0], error: null };

  const aliasMatches = workers.filter((worker) =>
    worker.label === requestedHost || worker.reachable_addr === requestedHost);
  if (aliasMatches.length > 1) {
    return {
      worker: undefined,
      error: `ambiguous deploy target "${requestedHost}" matches multiple registered workers; use the worker fingerprint`,
    };
  }
  return { worker: aliasMatches[0], error: null };
}

export function workerDeployHost(
  worker: {
    fp?: string;
    os?: string | null;
    label: string;
    reachable_addr: string | null;
  } | undefined,
  requestedHost: string,
): string {
  if (worker?.os === "win32" && worker.fp) return worker.fp;
  const reachableAddr = worker?.reachable_addr?.trim();
  if (reachableAddr) return reachableAddr;
  const label = worker?.label.trim();
  if (label) return label;
  return requestedHost;
}

function requireCoordinatorDeployAllowed(deps: ConnectDeps): void {
  if (deps.cfg.saasMode) {
    throw new ConnectError(
      "worker deployment is unavailable in managed mode",
      Code.PermissionDenied,
    );
  }
}

export function makeWorkerDeployHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, WorkerDeployMethods> {
  const deployJobScopes = new Map<string, { dashboardId: string }>();

  return {
    async *workersDeployOutput(req, ctx) {
      requireCoordinatorDeployAllowed(deps);
      const actor = requireDashboardAdmin(ctx.values);
      if (deployJobScopes.get(req.jobId)?.dashboardId !== actor.dashboardId) {
        throw new ConnectError("not found", Code.NotFound);
      }
      try {
        for await (const msg of deployOutput(req.jobId, ctx.signal)) {
          if (msg.kind === "line") {
            yield create(WorkersDeployOutputFrameSchema, {
              kind: "line",
              text: msg.text,
            });
          } else {
            yield create(WorkersDeployOutputFrameSchema, {
              kind: "done",
              exit: msg.exit ?? -1,
              error: msg.error ?? "",
            });
          }
        }
      } catch (error) {
        // A stalled reader tripped the bounded SSE queue: end with a terminal
        // frame so the SPA can explain the stop and reconnect for the tail.
        if (error instanceof SseQueueOverflowError) {
          yield create(WorkersDeployOutputFrameSchema, {
            kind: "done",
            exit: -1,
            error: "deploy output stream overflowed; reopen to resume",
          });
          return;
        }
        throw error;
      }
    },

    async workersDeployStart(req, ctx) {
      requireCoordinatorDeployAllowed(deps);
      const actor = requireDashboardAdmin(ctx.values);
      const workers = await deps.db
        .selectFrom("workers")
        .select(["fp", "os", "label", "reachable_addr"])
        .where("dashboard_id", "=", actor.dashboardId)
        .where("deleted_at_ms", "is", null)
        .where((expression) =>
          expression.or([
            expression("fp", "=", req.host),
            expression("label", "=", req.host),
            expression("reachable_addr", "=", req.host),
          ]),
        )
        .execute();
      const target = resolveWorkerDeployTarget(workers, req.host);
      if (target.error) {
        return create(WorkersDeployStartResponseSchema, {
          ok: false,
          jobId: "",
          error: target.error,
        });
      }
      const worker = target.worker;
      if (!worker) {
        return create(WorkersDeployStartResponseSchema, {
          ok: false,
          jobId: "",
          error: "worker not found",
        });
      }
      const host = workerDeployHost(worker, req.host);
      const result = worker.os === "win32"
        ? await startWindowsDeploy(
          worker.fp,
          req.expectedGitSha,
          req.expectedManifestSha256,
        )
        : startDeploy(host);
      if (result.ok && result.jobId) {
        const jobId = result.jobId;
        const scope = { dashboardId: actor.dashboardId };
        deployJobScopes.set(jobId, scope);
        setTimeout(() => {
          if (deployJobScopes.get(jobId) === scope) {
            deployJobScopes.delete(jobId);
          }
        }, DEPLOY_JOB_SCOPE_TTL_MS);
      }
      return create(WorkersDeployStartResponseSchema, {
        ok: result.ok,
        jobId: result.jobId ?? "",
        error: result.error ?? "",
      });
    },
  };
}
