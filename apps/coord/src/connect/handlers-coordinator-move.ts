import { Code, ConnectError, type ServiceImpl } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  CoordinatorMovePhase,
  CoordinatorMovePreflightResponseSchema,
  CoordinatorMoveStartResponseSchema,
  CoordinatorMoveStatusResponseSchema,
  CoordinatorService,
} from "@roost/shared/proto/coordinator_pb";
import { requireAuth } from "./auth-interceptor.ts";
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

export function makeCoordinatorMoveHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, CoordinatorMoveMethods> {
  return {
    async coordinatorMovePreflight(req, ctx) {
      requireAuth(ctx.values);
      const result = await requireMoveService(deps).preflight(req.targetWorkerFp);
      return create(CoordinatorMovePreflightResponseSchema, {
        eligible: result.eligible,
        sourceUrl: result.sourceUrl,
        targetUrl: result.targetUrl,
        blockers: result.blockers.map((item) => ({ code: item.code, message: item.message, workerFp: item.workerFp })),
      });
    },
    async coordinatorMoveStart(req, ctx) {
      requireAuth(ctx.values);
      try {
        return create(CoordinatorMoveStartResponseSchema, { handoffId: await requireMoveService(deps).start(req.targetWorkerFp) });
      } catch (error) {
        throw new ConnectError((error as Error).message, Code.FailedPrecondition);
      }
    },
    async coordinatorMoveStatus(req, ctx) {
      const state = requireMoveService(deps).status(req.handoffId);
      if (!state) throw new ConnectError("handoff not found", Code.NotFound);
      return create(CoordinatorMoveStatusResponseSchema, {
        phase: PHASE_TO_PROTO[state.phase], sourceUrl: state.source_url, targetUrl: state.target_url, error: state.error,
      });
    },
  };
}
