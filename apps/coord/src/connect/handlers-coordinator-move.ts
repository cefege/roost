// Thin Connect adapters for the coordinator-move lifecycle (preflight/start/
// status): organization-admin user operations delegate to deps.move; a worker
// may continue only as a server-verified handoff participant. A missing move
// service is FailedPrecondition, not a crash.
import { Code, ConnectError, type ServiceImpl } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  CoordinatorMovePhase,
  CoordinatorMovePreflightResponseSchema,
  CoordinatorMoveStartResponseSchema,
  CoordinatorMoveStatusResponseSchema,
  CoordinatorService,
} from "@roost/shared/proto/coordinator_pb";
import type { HandoffState } from "../coord-move/state.ts";
import { callerKey, requireOrganizationAdmin, requireWorker } from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";

const PHASE_TO_PROTO: Record<string, CoordinatorMovePhase> = {
  PREPARING_TARGET: CoordinatorMovePhase.PREPARING_TARGET,
  STAGING_WORKERS: CoordinatorMovePhase.STAGING_WORKERS,
  DRAINING_SOURCE: CoordinatorMovePhase.DRAINING_SOURCE,
  COPYING_STATE: CoordinatorMovePhase.COPYING_STATE,
  WAITING_FOR_WORKERS: CoordinatorMovePhase.WAITING_FOR_WORKERS,
  COMMITTING: CoordinatorMovePhase.COMMITTING,
  COMMITTED: CoordinatorMovePhase.COMMITTED,
  ROLLING_BACK: CoordinatorMovePhase.ROLLING_BACK,
  ROLLED_BACK: CoordinatorMovePhase.ROLLED_BACK,
  FAILED: CoordinatorMovePhase.FAILED,
};

type CoordinatorMoveMethods = "coordinatorMovePreflight" | "coordinatorMoveStart" | "coordinatorMoveStatus";

function requireMoveService(deps: ConnectDeps) {
  if (!deps.move) throw new ConnectError("coordinator move is not configured", Code.FailedPrecondition);
  return deps.move;
}

function requireMoveAllowed(deps: ConnectDeps): void {
  if (deps.cfg.saasMode) {
    throw new ConnectError("coordinator move is unavailable in managed mode", Code.PermissionDenied);
  }
}

export function makeCoordinatorMoveHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, CoordinatorMoveMethods> {
  return {
    async coordinatorMovePreflight(req, ctx) {
      requireMoveAllowed(deps);
      const actor = requireOrganizationAdmin(ctx.values);
      const result = await requireMoveService(deps).preflight(actor.dashboardId, req.targetWorkerFp);
      return create(CoordinatorMovePreflightResponseSchema, {
        eligible: result.eligible,
        sourceUrl: result.sourceUrl,
        targetUrl: result.targetUrl,
        blockers: result.blockers.map((item) => ({ code: item.code, message: item.message, workerFp: item.workerFp })),
      });
    },
    async coordinatorMoveStart(req, ctx) {
      requireMoveAllowed(deps);
      const actor = requireOrganizationAdmin(ctx.values);
      try {
        return create(CoordinatorMoveStartResponseSchema, {
          handoffId: await requireMoveService(deps).start(actor.dashboardId, req.targetWorkerFp),
        });
      } catch (error) {
        throw new ConnectError((error as Error).message, Code.FailedPrecondition);
      }
    },
    async coordinatorMoveStatus(req, ctx) {
      requireMoveAllowed(deps);
      const move = requireMoveService(deps);
      const principal = ctx.values.get(callerKey);
      let state: HandoffState | null;
      if (principal?.kind === "worker") {
        const worker = requireWorker(ctx.values);
        state = await move.statusForWorker(req.handoffId, worker.fingerprint);
      } else {
        const actor = requireOrganizationAdmin(ctx.values);
        state = move.status(actor.dashboardId, req.handoffId);
      }
      if (!state) throw new ConnectError("handoff not found", Code.NotFound);
      return create(CoordinatorMoveStatusResponseSchema, {
        phase: PHASE_TO_PROTO[state.phase], sourceUrl: state.source_url, targetUrl: state.target_url, error: state.error,
      });
    },
  };
}
