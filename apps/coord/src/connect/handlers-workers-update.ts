// Owns the coordinator-held keeper update boundary. It drains every
// channel-creating command, reauthorizes the worker, snapshots every open
// session in canonical order, then awaits the authenticated worker action.
// Only an explicitly forced maintenance refresh may cross live sessions.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, type ServiceImpl } from "@connectrpc/connect";
import {
  JournaledKeeperUpdateV1Schema,
  KeeperCoordinatorOpenSessionIdsSchema,
  keeperUpdateOutcomeMatchesAction,
  type JournaledKeeperUpdateV1,
} from "@roost/shared/keeper-update";
import {
  CoordinatorService,
  WorkersPrepareKeeperUpdateResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { log } from "@roost/shared/log";
import { requireAccountDevice, resolveCallerPrincipal } from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";
import { sendKeeperUpdatePreparation } from "./worker-service.ts";

type WorkerUpdateMethods = "workersPrepareKeeperUpdate";

interface WorkerKeeperPreparationResult {
  outcome: string;
  keeper_pid?: number;
  keeper_epoch?: string;
  binding_digest?: string;
}

function parseJournaledUpdate(
  encoded: string | undefined,
  direction: string,
  maintenance: boolean,
  forceLive: boolean,
): { update: JournaledKeeperUpdateV1 | null; direction: "source" | "target" | null } {
  // force_live authorizes destroying live PTYs. It is meaningful only on the
  // maintenance path, which carries no journaled envelope, so no replayed or
  // hand-edited journal can ever arrive holding it.
  if (forceLive && !maintenance) {
    throw new ConnectError(
      "keeper force-live requires the maintenance path",
      Code.InvalidArgument,
    );
  }
  if (maintenance) {
    if (encoded !== undefined || direction !== "") {
      throw new ConnectError(
        "keeper maintenance cannot carry a journaled update",
        Code.InvalidArgument,
      );
    }
    return { update: null, direction: null };
  }
  if (encoded === undefined || encoded.length === 0 || encoded.length > 32 * 1024) {
    throw new ConnectError("journaled keeper update is required", Code.InvalidArgument);
  }
  if (direction !== "source" && direction !== "target") {
    throw new ConnectError("keeper update direction is invalid", Code.InvalidArgument);
  }
  let value: unknown;
  try {
    value = JSON.parse(encoded);
  } catch {
    throw new ConnectError("journaled keeper update is malformed", Code.InvalidArgument);
  }
  const parsed = JournaledKeeperUpdateV1Schema.safeParse(value);
  if (!parsed.success) {
    throw new ConnectError(
      "journaled keeper update is malformed",
      Code.InvalidArgument,
    );
  }
  return { update: parsed.data, direction };
}

export function makeWorkerUpdateHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, WorkerUpdateMethods> {
  return {
    async workersPrepareKeeperUpdate(req, ctx) {
      const caller = requireAccountDevice(ctx.values);
      if (!/^[0-9a-f]{64}$/.test(req.workerFp)) {
        throw new ConnectError("worker fingerprint is invalid", Code.InvalidArgument);
      }
      const requested = parseJournaledUpdate(
        req.journaledUpdateJson,
        req.direction,
        req.maintenance,
        req.forceLive,
      );
      const lease = await deps.writeGate.acquireExclusive(
        `keeper-update:${caller.fingerprint}:${req.workerFp}`,
      );
      try {
        // The fence can queue behind another keeper update, so the caller's
        // credential is re-read after the wait: a key or device revoked while
        // queued must not proceed on a stale admission.
        const currentCaller = await resolveCallerPrincipal(deps.db, {
          fingerprint: caller.fingerprint,
          label: caller.label,
        });
        if (
          !currentCaller
          || (currentCaller.kind !== "account-device"
            && currentCaller.kind !== "legacy-self-hosted")
        ) {
          throw new ConnectError("authentication required", Code.Unauthenticated);
        }
        const worker = await deps.db.selectFrom("workers")
          .select("fp")
          .where("fp", "=", req.workerFp)
          .where("deleted_at_ms", "is", null)
          .executeTakeFirst();
        if (!worker) throw new ConnectError("worker not found", Code.NotFound);
        deps._onKeeperUpdateFinalEmptyRecheck?.();
        const openSessions = await deps.db.selectFrom("sessions")
          .select("id")
          .where("worker_fp", "=", worker.fp)
          .where("status", "=", "open")
          .orderBy("id", "asc")
          .execute();
        const parsedSessionIds = KeeperCoordinatorOpenSessionIdsSchema.safeParse(
          openSessions.map(session => session.id),
        );
        if (!parsedSessionIds.success) {
          throw new ConnectError(
            "coordinator open-session proof is malformed",
            Code.DataLoss,
          );
        }
        const coordinatorOpenSessionIds = parsedSessionIds.data;
        const requiresEmpty = (req.maintenance && !req.forceLive)
          || requested.update?.admission.required_action === "replace-empty";
        if (requiresEmpty && coordinatorOpenSessionIds.length !== 0) {
          throw new ConnectError(
            req.maintenance
              ? "keeper maintenance blocked by live sessions"
              : "keeper replacement blocked by live sessions",
            Code.FailedPrecondition,
          );
        }
        if (req.forceLive) {
          log.warn("coord", "keeper_maintenance_force_live_authorized", {
            worker_fp: worker.fp,
            device_fingerprint: caller.fingerprint,
            coordinator_open_sessions: coordinatorOpenSessionIds.length,
          });
        }
        const rawResult = await sendKeeperUpdatePreparation(worker.fp, {
          journaledUpdateJson: requested.update
            ? JSON.stringify(requested.update)
            : undefined,
          direction: requested.direction ?? undefined,
          maintenance: req.maintenance,
          forceLive: req.forceLive,
          coordinatorOpenSessionIds,
        });
        const result = rawResult as Partial<WorkerKeeperPreparationResult>;
        const outcome = result.outcome;
        if (typeof outcome !== "string" || outcome.length === 0) {
          throw new ConnectError("worker returned malformed keeper proof", Code.DataLoss);
        }
        const requestedAction = req.maintenance
          ? "maintenance"
          : requested.update!.admission.required_action;
        const outcomeMatchesAction = keeperUpdateOutcomeMatchesAction(
          requestedAction,
          outcome,
        );
        if (!outcomeMatchesAction) {
          throw new ConnectError(
            "worker returned keeper proof for a different action",
            Code.DataLoss,
          );
        }
        const identity = result.keeper_pid !== undefined
          || result.keeper_epoch !== undefined
          || result.binding_digest !== undefined;
        if (requestedAction === "preserve"
          && (!Number.isSafeInteger(result.keeper_pid)
            || result.keeper_pid! <= 0
            || typeof result.keeper_epoch !== "string"
            || !/^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i
              .test(result.keeper_epoch)
            || typeof result.binding_digest !== "string"
            || !/^[0-9a-f]{64}$/.test(result.binding_digest))) {
          throw new ConnectError("worker returned malformed keeper identity", Code.DataLoss);
        }
        if (requestedAction !== "preserve" && identity) {
          throw new ConnectError("worker returned unexpected keeper identity", Code.DataLoss);
        }
        return create(WorkersPrepareKeeperUpdateResponseSchema, {
          outcome,
          keeperPid: requestedAction === "preserve" ? BigInt(result.keeper_pid!) : undefined,
          keeperEpoch: requestedAction === "preserve" ? result.keeper_epoch : undefined,
          bindingDigest: requestedAction === "preserve" ? result.binding_digest : undefined,
        });
      } finally {
        lease.release();
      }
    },
  };
}
