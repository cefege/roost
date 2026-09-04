// SessionsSpawn RPC handler: forwards a coordinator-selected session UUID to
// the target worker and dedupes concurrent spawns within the selected dashboard.
// Exact duplicates share one result while signature conflicts reject before
// membership changes. Worker replies must match the reserved identity.
import { create } from "@bufbuild/protobuf";
import { Code, ConnectError } from "@connectrpc/connect";
import { randomUUID } from "node:crypto";
import {
  SessionsSpawnResponseSchema,
  type SessionsSpawnRequest,
  type SessionsSpawnResponse,
} from "@roost/shared/proto/coordinator_pb";
import { asSessionId, type ClientControlFrame } from "@roost/shared/wire";
import { createPendingRpc, rejectPendingRpc } from "../router/pending-rpcs.ts";
import type { ConnectDeps } from "./router.ts";
import type { AccountDeviceCaller, DashboardActor } from "./auth-interceptor.ts";
import { getWorkerHubSocket, sendBrowserCommand } from "./worker-service.ts";
import { sendBrowserCmd } from "./router-helpers.ts";
import {
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
}

/** Spawn dimensions are only an initial PTY-size hint. Live geometry starts
 * when a mounted TerminalViewCommand joins through TerminalViewHub. */
export function spawnFrameFor(req: {
  kind: string;
  folder: string;
  cols?: number;
  rows?: number;
  sessionId?: string;
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
      };
    default:
      throw new ConnectError(`unknown session kind ${req.kind}`, Code.InvalidArgument);
  }
}

export async function handleSessionsSpawn(
  deps: ConnectDeps,
  req: SessionsSpawnRequest,
  caller: AccountDeviceCaller,
  actor: DashboardActor,
  tabId: string | undefined,
): Promise<SessionsSpawnResponse> {
  const callerKey = tabId ? `${caller.fingerprint}:${tabId}` : caller.fingerprint;
  if (req.sessionId && !CALLER_SESSION_ID_RE.test(req.sessionId)) {
    throw new ConnectError("session_id must be a canonical UUID", Code.InvalidArgument);
  }
  const sessionId = req.sessionId || randomUUID();

  const worker = await deps.db.selectFrom("workers")
    .select("fp")
    .where("fp", "=", req.workerFp)
    .where("dashboard_id", "=", actor.dashboardId)
    .where("deleted_at_ms", "is", null)
    .executeTakeFirst();
  if (!worker) throw new ConnectError("worker not found", Code.NotFound);

  const signature: PendingSpawnSignature = {
    callerKey,
    workerFp: worker.fp,
    dashboardId: actor.dashboardId,
    kind: req.kind,
    folder: req.folder,
    cols: req.cols,
    rows: req.rows,
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
    const existing = await deps.db.selectFrom("sessions").select("id")
      .where("id", "=", sessionId)
      .executeTakeFirst();
    if (existing) {
      rejectPendingSpawn(
        actor.dashboardId,
        sessionId,
        new ConnectError("session_id already exists", Code.AlreadyExists),
        true,
      );
    } else {
      const socket = getWorkerHubSocket(worker.fp);
      if (!socket) {
        rejectPendingSpawn(
          actor.dashboardId,
          sessionId,
          new ConnectError(
            `worker ${worker.fp.slice(0, 12)} not connected`,
            Code.FailedPrecondition,
          ),
          true,
        );
      } else {
        const pending = createPendingRpc<WorkerSpawnResult>(15_000, worker.fp);
        void pending.promise.then((data) => {
          if (
            data.session_id !== sessionId
            || !Number.isSafeInteger(data.channel_id)
            || data.channel_id <= 0
          ) {
            rejectPendingSpawn(
              actor.dashboardId,
              sessionId,
              new ConnectError("worker returned an invalid spawn identity", Code.DataLoss),
              true,
            );
            return;
          }
          resolvePendingSpawn(actor.dashboardId, sessionId, {
            sessionId,
            channelId: data.channel_id,
          });
        }, (error: Error) => {
          const definite = error instanceof ConnectError && error.code === Code.Internal;
          rejectPendingSpawn(actor.dashboardId, sessionId, error, definite);
        });
        const sent = sendBrowserCommand(worker.fp, {
          browser_id: caller.fingerprint,
          viewer_id: callerKey,
          request_id: pending.request_id,
          frame: spawnFrameFor({ ...req, sessionId }),
        });
        if (!sent) {
          const error = new ConnectError("worker send failed", Code.Unavailable);
          rejectPendingRpc(pending.request_id, error.message, worker.fp);
          rejectPendingSpawn(actor.dashboardId, sessionId, error, true);
        }
      }
    }
  }

  const result: PendingSpawnResult = await reservation.promise;
  return create(SessionsSpawnResponseSchema, {
    sessionId: result.sessionId,
    channelId: result.channelId,
  });
}
