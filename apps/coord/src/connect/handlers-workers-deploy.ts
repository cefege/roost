// Owns worker deployment RPCs plus their output stream. handlers-workers.ts
// spreads these into the single router.service() literal. Job identity,
// buffered output, and Windows update admission belong to deploy-jobs.ts and
// windows-update-manifest.ts; this file only authorizes and adapts them.

import { create } from "@bufbuild/protobuf";
import type { ServiceImpl } from "@connectrpc/connect";
import {
  type CoordinatorService,
  WorkersDeployOutputFrameSchema,
  WorkersDeployStartResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { deployOutput, startDeploy } from "../deploy-jobs.ts";
import { SseQueueOverflowError } from "../sse.ts";
import { startWindowsDeploy } from "../windows-update-manifest.ts";
import { requireAccountDevice } from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";

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

export function makeWorkerDeployHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, WorkerDeployMethods> {
  return {
    async *workersDeployOutput(req, ctx) {
      requireAccountDevice(ctx.values);
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
      requireAccountDevice(ctx.values);
      const workers = await deps.db
        .selectFrom("workers")
        .select(["fp", "os", "label", "reachable_addr"])
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
      return create(WorkersDeployStartResponseSchema, {
        ok: result.ok,
        jobId: result.jobId ?? "",
        error: result.error ?? "",
      });
    },
  };
}
