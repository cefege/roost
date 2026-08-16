import { create } from "@bufbuild/protobuf";
import { Code, ConnectError } from "@connectrpc/connect";
import {
  SessionsSpawnResponseSchema,
  type SessionsSpawnRequest,
  type SessionsSpawnResponse,
} from "@roost/shared/proto/coordinator_pb";
import { asSessionId, type ClientControlFrame } from "@roost/shared/wire";
import { createPendingRpc, rejectPendingRpc } from "../router/pending-rpcs.ts";
import type { ConnectDeps } from "./router.ts";
import type { Caller } from "./auth-interceptor.ts";
import { getWorkerHubSocket, sendBrowserCommand } from "./worker-service.ts";
import { sendBrowserCmd } from "./router-helpers.ts";
import { mutateCellSubscription } from "./cell-subscriptions.ts";
import {
  configurePendingSpawnPreclaim,
  rejectPendingSpawn,
  reservePendingSpawn,
  resolvePendingSpawn,
  type PendingSpawnResult,
  type PendingSpawnSignature,
} from "./pending-spawns.ts";

const CALLER_SESSION_ID_RE =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

interface WorkerSpawnResult {
  session_id: string;
  channel_id: number;
  initial_viewport_preclaimed?: boolean;
  effective_client_seq?: number;
}

/** SessionsSpawnRequest → the worker control frame for its kind. */
export function spawnFrameFor(req: {
  kind: string;
  folder: string;
  cols?: number;
  rows?: number;
  sessionId?: string;
  preclaimInitialViewport?: boolean;
  effectiveClientSeq?: bigint;
}): ClientControlFrame {
  const sid = req.sessionId ? { session_id: asSessionId(req.sessionId) } : {};
  switch (req.kind) {
    case "shell":
      return {
        kind: "spawn-shell",
        folder: req.folder,
        cols: req.cols,
        rows: req.rows,
        ...sid,
        ...(req.preclaimInitialViewport
          ? {
            preclaim_initial_viewport: true,
            initial_viewport_client_seq: Number(req.effectiveClientSeq),
          }
          : {}),
      };
    default:
      throw new ConnectError(`unknown session kind ${req.kind}`, Code.InvalidArgument);
  }
}

export async function handleSessionsSpawn(
  deps: ConnectDeps,
  req: SessionsSpawnRequest,
  caller: Caller,
  tabId: string | undefined,
): Promise<SessionsSpawnResponse> {
  const viewerKey = tabId
    ? `${caller.fingerprint}:${tabId}`
    : caller.fingerprint;
  const preclaim = req.preclaimInitialViewport;
  if (req.sessionId && !CALLER_SESSION_ID_RE.test(req.sessionId)) {
    throw new ConnectError("session_id must be a canonical UUID", Code.InvalidArgument);
  }
  if (preclaim && !req.sessionId) {
    throw new ConnectError("preclaimed spawn requires caller session_id", Code.InvalidArgument);
  }
  if (
    preclaim
    && (
      !tabId
      || req.cols === undefined
      || req.rows === undefined
      || req.cols <= 0
      || req.rows <= 0
      || req.initialViewportClientSeq <= 0n
      || req.initialViewportClientSeq > BigInt(Number.MAX_SAFE_INTEGER)
    )
  ) {
    throw new ConnectError(
      "preclaimed spawn requires an authenticated tab, positive mounted size, and safe client sequence",
      Code.InvalidArgument,
    );
  }

  // Public/non-SPA callers that let the worker mint the id retain unary behavior.
  if (!req.sessionId) {
    const sock = getWorkerHubSocket(req.workerFp);
    if (!sock) throw new ConnectError(`worker ${req.workerFp.slice(0, 12)} not connected`, Code.FailedPrecondition);
    const pending = createPendingRpc<WorkerSpawnResult>(15_000, req.workerFp);
    sendBrowserCmd(sock, caller, pending.request_id, spawnFrameFor(req));
    const data = await pending.promise;
    return create(SessionsSpawnResponseSchema, {
      sessionId: data.session_id,
      channelId: data.channel_id,
    });
  }

  const sessionId = req.sessionId;
  const signature: PendingSpawnSignature = {
    callerKey: viewerKey,
    workerFp: req.workerFp,
    kind: req.kind,
    folder: req.folder,
    cols: req.cols,
    rows: req.rows,
    preclaimInitialViewport: preclaim,
    requestedClientSeq: req.initialViewportClientSeq,
  };
  const reservation = reservePendingSpawn(sessionId, signature);
  if (reservation.kind === "conflict") {
    throw new ConnectError(
      "session_id is already pending with different caller or parameters",
      Code.AlreadyExists,
    );
  }
  if (reservation.kind === "capacity") {
    throw new ConnectError("too many pending session spawns", Code.ResourceExhausted);
  }

  if (reservation.kind === "new") {
    // Reservation precedes the DB await, closing the two-request collision race.
    const existing = await deps.db.selectFrom("sessions").select(["id"])
      .where("id", "=", sessionId).executeTakeFirst();
    if (existing) {
      rejectPendingSpawn(
        sessionId,
        new ConnectError("session_id already exists", Code.AlreadyExists),
        true,
      );
    } else {
      const sock = getWorkerHubSocket(req.workerFp);
      if (!sock) {
        rejectPendingSpawn(
          sessionId,
          new ConnectError(`worker ${req.workerFp.slice(0, 12)} not connected`, Code.FailedPrecondition),
          true,
        );
      } else {
        let effectiveClientSeq = 0n;
        let installPreclaim = preclaim;
        if (preclaim) {
          const mutation = mutateCellSubscription(
            viewerKey,
            sessionId,
            true,
            req.initialViewportClientSeq,
          );
          if (!mutation) {
            // A newer local intent (typically a hide/withdraw during the 100ms
            // measurement window) superseded this preclaim. Spawn normally but
            // do not revive stale membership; reveal will claim a newer seq.
            installPreclaim = false;
          } else {
            effectiveClientSeq = mutation.effectiveClientSeq;
            configurePendingSpawnPreclaim(
              sessionId,
              effectiveClientSeq,
              mutation.rollback,
            );
          }
        }

        const pending = createPendingRpc<WorkerSpawnResult>(15_000, req.workerFp);
        void pending.promise.then((data) => {
          if (
            data.session_id !== sessionId
            || !Number.isSafeInteger(data.channel_id)
            || data.channel_id <= 0
          ) {
            rejectPendingSpawn(
              sessionId,
              new ConnectError("worker returned an invalid spawn identity", Code.DataLoss),
              true,
            );
            return;
          }
          const workerPreclaimed = installPreclaim
            && data.initial_viewport_preclaimed === true;
          if (
            workerPreclaimed
            && (
              typeof data.effective_client_seq !== "number"
              || !Number.isSafeInteger(data.effective_client_seq)
              || BigInt(data.effective_client_seq) !== effectiveClientSeq
            )
          ) {
            rejectPendingSpawn(
              sessionId,
              new ConnectError("worker returned a mismatched viewport sequence", Code.DataLoss),
              true,
            );
            return;
          }
          resolvePendingSpawn(sessionId, {
            sessionId,
            channelId: data.channel_id,
            initialViewportPreclaimed: workerPreclaimed,
            effectiveClientSeq: workerPreclaimed ? effectiveClientSeq : 0n,
          });
        }, (error: Error) => {
          const definite = error instanceof ConnectError
            && error.code === Code.Internal;
          rejectPendingSpawn(sessionId, error, definite);
        });
        const sent = sendBrowserCommand(req.workerFp, {
          browser_id: caller.fingerprint,
          viewer_id: viewerKey,
          request_id: pending.request_id,
          frame: spawnFrameFor({
            ...req,
            preclaimInitialViewport: installPreclaim,
            effectiveClientSeq,
          }),
        });
        if (!sent) {
          const error = new ConnectError("worker send failed", Code.Unavailable);
          rejectPendingRpc(pending.request_id, error.message);
          rejectPendingSpawn(sessionId, error, true);
        }
      }
    }
  }

  const result: PendingSpawnResult = await reservation.promise;
  return create(SessionsSpawnResponseSchema, {
    sessionId: result.sessionId,
    channelId: result.channelId,
    initialViewportPreclaimed: result.initialViewportPreclaimed,
    effectiveClientSeq: result.effectiveClientSeq,
  });
}
