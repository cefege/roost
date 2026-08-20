// Terminal viewport control: one browser viewport intent per lane turn. Split
// out of session-control.ts, which is now a re-export barrel; the lane, the
// viewer identity, and session route resolution are shared with
// input-control.ts through terminal-control-lane.ts, and the coordinator-local
// recovery replay lives in barrier-repair-replay.ts.

import {
  TerminalViewportStatus,
  TerminalWritePhase,
} from "@roost/shared/proto/worker_transport_pb";
import type { ConnectDeps } from "./router.ts";
import { isBarrierRepairMarked } from "../byte-hub-barrier-repair.ts";
import {
  currentCellSubscriptionSeq,
  mutateCellSubscription,
  type CellSubscriptionMutation,
} from "./cell-subscriptions.ts";
import {
  currentViewerSeq,
  mutateViewer,
  type ViewerMutation,
} from "./viewer-tracker.ts";
import {
  sendTerminalViewportRequest,
  startHopDeadline,
  VIEWPORT_CONTROL_TIMEOUT_MS,
  type HopDeadline,
} from "./worker-send.ts";
import {
  enqueueLane,
  laneKey,
  resolveSessionRoute,
  type TerminalControlGeneration,
  type TerminalViewerIdentity,
} from "./terminal-control-lane.ts";
import type { WriteLease } from "../coord-move/write-gate.ts";

const COMMITTED_VIEWPORT_CACHE_CAP = 8_192;

interface CommittedViewport {
  browserClientSeq: bigint;
  cellMutation: CellSubscriptionMutation;
  viewerMutation: ViewerMutation;
  cols: number;
  rows: number;
  cause: number;
  heldCellSeq: bigint;
  result: Extract<ViewportControlResult, { status: "accepted" }>;
}

// Only a matching typed worker COMMITTED result enters this bounded cache.
// Exact replay is allowed only while both installed mutation identities remain
// current, including across legacy zero-sequence withdrawal semantics.
const committedViewports = new Map<string, CommittedViewport>();

export type ViewportControlResult =
  | {
      status: "accepted";
      sessionId: string;
      clientSeq: bigint;
      channelResizeSeq: bigint;
      cols: number;
      rows: number;
      resized: boolean;
    }
  | {
      status: "rejected";
      sessionId: string;
      clientSeq: bigint;
      reason: string;
      sequenceFloor?: bigint;
    }
  | {
      status: "ambiguous";
      sessionId: string;
      clientSeq: bigint;
      reason: string;
    };

export interface ViewportControlCommand {
  identity: TerminalViewerIdentity;
  sessionId: string;
  clientSeq: bigint;
  cols: number;
  rows: number;
  cause: number;
  heldCellSeq?: bigint;
  socketGeneration?: TerminalControlGeneration;
  /** Monotonic hop budget shared by the lane wait and the worker send.
   * Injected by tests; production starts it at entry. */
  deadline?: HopDeadline;
}

const viewportRejected = (
  command: Pick<ViewportControlCommand, "sessionId" | "clientSeq">,
  reason: string,
  sequenceFloor?: bigint,
): ViewportControlResult => ({
  status: "rejected",
  sessionId: command.sessionId,
  clientSeq: command.clientSeq,
  reason: reason.slice(0, 200),
  ...(sequenceFloor === undefined ? {} : { sequenceFloor }),
});

const viewportAmbiguous = (
  command: Pick<ViewportControlCommand, "sessionId" | "clientSeq">,
  reason: string,
): ViewportControlResult => ({
  status: "ambiguous",
  sessionId: command.sessionId,
  clientSeq: command.clientSeq,
  reason: reason.slice(0, 200),
});

/** Apply one viewport command under the shared per-viewer/session lane. Cell
 * and viewer membership become provisional before transport admission. A
 * definite non-admission, a pre-send budget expiry, or a typed REJECTED result
 * whose phase proves the keeper never wrote rolls both back by identity; every
 * other post-admission outcome remains TTL-backed and is reported as ambiguous
 * for a newer monotonic intent to reconcile. */
export function processViewportControl(
  deps: ConnectDeps,
  command: ViewportControlCommand,
): Promise<ViewportControlResult> {
  const socketGeneration = command.socketGeneration ?? 0;
  // The budget starts here, before the lane wait, so a command that queues
  // behind a slow predecessor spends its own time rather than being handed a
  // fresh full budget at send.
  const deadline = command.deadline ?? startHopDeadline(VIEWPORT_CONTROL_TIMEOUT_MS);
  return enqueueLane(
    command.identity.viewerKey,
    command.sessionId,
    socketGeneration,
    async (releaseLane) => {
      let lease: WriteLease | null = null;
      let cellMutation: CellSubscriptionMutation | null = null;
      let viewerMutation: ViewerMutation | null = null;
      let admitted = false;
      try {
        lease = deps.move?.gate.acquire() ?? null;
        const route = await resolveSessionRoute(deps.db, command.sessionId);
        if (!route) return viewportRejected(command, "unknown session");

        // Barrier repair is coordinator-local and exact by route: the
        // announcement barrier dropped this (worker, session, channel)'s cells,
        // so the browser's held sequence cannot prove it is current even though
        // the browser has no way to know that. Override the worker-bound value
        // to zero — no browser protocol field — until a full frame publishes for
        // this exact route; later claims carry the browser's own value again.
        const heldCellSeq = isBarrierRepairMarked(
          route.workerFp,
          command.sessionId,
          route.channel,
        )
          ? 0n
          : command.heldCellSeq ?? 0n;

        const subscribes = command.cause === 5
          || (command.cols > 0 && command.rows > 0);
        const refreshEqual = command.cause === 6;
        cellMutation = mutateCellSubscription(
          command.identity.viewerKey,
          command.sessionId,
          subscribes,
          command.clientSeq,
          refreshEqual,
        );
        if (!cellMutation) {
          const committed = committedViewports.get(
            laneKey(command.identity.viewerKey, command.sessionId),
          );
          if (
            committed
            && committed.browserClientSeq === command.clientSeq
            && committed.cols === command.cols
            && committed.rows === command.rows
            && committed.cause === command.cause
            && committed.heldCellSeq === heldCellSeq
            && committed.cellMutation.isCurrent()
            && committed.viewerMutation.isCurrent()
          ) {
            return committed.result;
          }
          return viewportRejected(
            command,
            "stale or conflicting viewport intent",
            currentCellSubscriptionSeq(
              command.identity.viewerKey,
              command.sessionId,
            ) ?? undefined,
          );
        }

        const effectiveClientSeq = cellMutation.effectiveClientSeq;
        viewerMutation = mutateViewer(
          command.sessionId,
          command.identity.viewerKey,
          subscribes,
          command.cols,
          command.rows,
          command.clientSeq,
          refreshEqual,
          command.identity.clientIp,
          command.identity.callerFingerprint,
        );
        if (
          !viewerMutation
          || viewerMutation.effectiveClientSeq !== effectiveClientSeq
        ) {
          viewerMutation?.rollback();
          cellMutation.rollback();
          return viewportRejected(
            command,
            "stale or conflicting viewer intent",
            currentViewerSeq(
              command.sessionId,
              command.identity.viewerKey,
            ) ?? undefined,
          );
        }

        const workerCall = sendTerminalViewportRequest(route.workerFp, {
          sessionId: command.sessionId,
          viewerId: command.identity.viewerKey,
          clientSeq: effectiveClientSeq,
          cols: command.cols,
          rows: command.rows,
          cause: command.cause,
          heldCellSeq,
        }, deadline);
        admitted = workerCall.admitted;
        if (!admitted) {
          void workerCall.result.catch(() => undefined);
          viewerMutation.rollback();
          cellMutation.rollback();
          // Neither branch reached the socket, so no PTY state can exist and a
          // retry cannot duplicate: both are definite rejections.
          return viewportRejected(
            command,
            workerCall.expired
              ? "viewport budget expired before worker send"
              : "worker unavailable",
          );
        }
        // The frame is on the current worker socket, so the receive order the
        // worker's keeper-admission lane depends on is already fixed. Let the
        // next command send while this result finalizes.
        releaseLane();

        const result = await workerCall.result;
        if (
          result.requestId !== workerCall.requestId
          || result.sessionId !== command.sessionId
          || result.clientSeq !== effectiveClientSeq
        ) {
          return viewportAmbiguous(command, "mismatched worker viewport result");
        }

        switch (result.status) {
          case TerminalViewportStatus.REJECTED:
            // A rejection may only unwind provisional membership when the
            // worker proves it stopped before the keeper write. Any other
            // phase means a mutation could exist, so it stays ambiguous and
            // convergence is left to a newer claim rather than a rollback.
            if (result.phase !== TerminalWritePhase.PRE_WRITE) {
              return viewportAmbiguous(
                command,
                result.reason || "worker rejected without proving no resize",
              );
            }
            viewerMutation.rollback();
            cellMutation.rollback();
            return viewportRejected(
              command,
              result.reason || "keeper rejected viewport",
              result.sequenceFloor,
            );
          case TerminalViewportStatus.AMBIGUOUS:
            return viewportAmbiguous(
              command,
              result.reason || "worker could not prove viewport outcome",
            );
          case TerminalViewportStatus.UNSPECIFIED:
            return viewportAmbiguous(
              command,
              result.reason || "worker returned no viewport outcome",
            );
          case TerminalViewportStatus.COMMITTED: {
            if (!cellMutation.isCurrent() || !viewerMutation.isCurrent()) {
              return viewportAmbiguous(
                command,
                "viewport ownership changed before worker commit",
              );
            }
            const accepted: Extract<ViewportControlResult, { status: "accepted" }> = {
              status: "accepted",
              sessionId: command.sessionId,
              clientSeq: command.clientSeq,
              channelResizeSeq: result.channelResizeSeq,
              cols: result.cols,
              rows: result.rows,
              resized: result.resized,
            };
            const committedKey = laneKey(
              command.identity.viewerKey,
              command.sessionId,
            );
            if (
              !committedViewports.has(committedKey)
              && committedViewports.size >= COMMITTED_VIEWPORT_CACHE_CAP
            ) {
              const oldestKey = committedViewports.keys().next().value;
              if (oldestKey !== undefined) committedViewports.delete(oldestKey);
            }
            // Reinsertion makes replacement order deterministic for bounded
            // oldest-first eviction without another recency registry.
            committedViewports.delete(committedKey);
            committedViewports.set(committedKey, {
              browserClientSeq: command.clientSeq,
              cellMutation,
              viewerMutation,
              cols: command.cols,
              rows: command.rows,
              cause: command.cause,
              heldCellSeq,
              result: accepted,
            });
            return accepted;
          }
          default:
            return viewportAmbiguous(
              command,
              result.reason || "worker returned an unknown viewport outcome",
            );
        }
      } catch (error) {
        if (admitted) {
          return viewportAmbiguous(
            command,
            error instanceof Error ? error.message : "viewport result unavailable",
          );
        }
        viewerMutation?.rollback();
        cellMutation?.rollback();
        return viewportRejected(
          command,
          error instanceof Error ? error.message : "coordinator is not write-active",
        );
      } finally {
        lease?.release();
      }
    },
    () => viewportRejected(command, "generation closed or control queue full"),
  );
}
