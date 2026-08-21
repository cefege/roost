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
  caller: Caller,
  tabId: string | undefined,
): Promise<SessionsSpawnResponse> {
  const callerKey = tabId ? `${caller.fingerprint}:${tabId}` : caller.fingerprint;
  if (req.sessionId && !CALLER_SESSION_ID_RE.test(req.sessionId)) {
    throw new ConnectError("session_id must be a canonical UUID", Code.InvalidArgument);
  }

  if (!req.sessionId) {
    const socket = getWorkerHubSocket(req.workerFp);
    if (!socket) {
      throw new ConnectError(
        `worker ${req.workerFp.slice(0, 12)} not connected`,
        Code.FailedPrecondition,
      );
    }
    const pending = createPendingRpc<WorkerSpawnResult>(15_000, req.workerFp);
    sendBrowserCmd(socket, caller, pending.request_id, spawnFrameFor(req));
    const data = await pending.promise;
    return create(SessionsSpawnResponseSchema, {
      sessionId: data.session_id,
      channelId: data.channel_id,
    });
  }

  const sessionId = req.sessionId;
  const signature: PendingSpawnSignature = {
    callerKey,
    workerFp: req.workerFp,
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
    const existing = await deps.db.selectFrom("sessions").select(["id"])
      .where("id", "=", sessionId).executeTakeFirst();
    if (existing) {
      rejectPendingSpawn(
        sessionId,
        new ConnectError("session_id already exists", Code.AlreadyExists),
        true,
      );
    } else {
      const socket = getWorkerHubSocket(req.workerFp);
      if (!socket) {
        rejectPendingSpawn(
          sessionId,
          new ConnectError(
            `worker ${req.workerFp.slice(0, 12)} not connected`,
            Code.FailedPrecondition,
          ),
          true,
        );
      } else {
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
          resolvePendingSpawn(sessionId, {
            sessionId,
            channelId: data.channel_id,
          });
        }, (error: Error) => {
          const definite = error instanceof ConnectError && error.code === Code.Internal;
          rejectPendingSpawn(sessionId, error, definite);
        });
        const sent = sendBrowserCommand(req.workerFp, {
          browser_id: caller.fingerprint,
          viewer_id: callerKey,
          request_id: pending.request_id,
          frame: spawnFrameFor(req),
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
  });
}
