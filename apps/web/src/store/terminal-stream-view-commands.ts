// View commands keep viewport revisions and acknowledgements ordered across Sync generations.
// The view registry calls this module whenever local geometry or activity changes.
// Sync view-state frames return here before a stream is installed in the session replica.
// ACK deadlines use the same generation identity as terminal liveness recovery.

import { create } from "@bufbuild/protobuf";
import { signal } from "@roost/shared/diag";
import {
  TerminalViewCommandSchema,
  TerminalViewStatus,
  type TerminalViewStateFrame,
} from "@roost/shared/proto/sync_pb";
import {
  TERMINAL_VIEW_LEASE_MS,
  isTerminalGeometry,
  isTerminalUuid,
} from "@roost/shared/viewport";
import { isPageVisible } from "../lib/pageVisible.ts";
import {
  currentSyncV2TerminalState,
  requestSyncGenerationRecovery,
  sendSyncV2Command,
  type SyncV2TerminalState,
} from "./sync.ts";
import {
  armTerminalForegroundIdleProbe,
  clearTerminalSessionLiveness,
  installExpectedTerminalStream,
  sendLatchedTerminalResync,
  terminalGenerationMatches,
  terminalGenerationToken,
} from "./terminal-stream-replica.ts";
import {
  emitTerminalViewStatus,
  terminalSessions,
} from "./terminal-stream-state.ts";
import type {
  TerminalGenerationToken,
  TerminalOutboundCommand,
  TerminalSessionReplica,
  TerminalViewIntent,
  TerminalViewRecord,
} from "./terminal-stream-types.ts";

export function clearViewAck(view: TerminalViewRecord): void {
  clearTimeout(view.viewAckTimer ?? undefined);
  view.viewAckTimer = null;
  view.pendingViewAckAtMs = null;
  view.pendingViewAckGeneration = null;
  view.pendingViewAckRevision = null;
}

export function hasActiveTerminalView(session: TerminalSessionReplica): boolean {
  for (const view of session.handles.values()) {
    if (!view.disposed && view.desired?.active) return true;
  }
  return false;
}

export function publishIntent(
  view: TerminalViewRecord,
  intent: TerminalViewIntent,
  sync = currentSyncV2TerminalState(),
): boolean {
  const statusCurrent = view.status?.revision === intent.revision;
  if (view.disposed || !sync?.ready) {
    if (!view.disposed && !statusCurrent) {
      emitTerminalViewStatus(view, {
        status: "pending",
        revision: intent.revision,
        active: intent.active,
      });
    }
    return false;
  }
  if (!terminalGenerationMatches(view.session.generation, sync)) {
    clearTerminalSessionLiveness(view.session, "generation_reset");
    view.session.generation = terminalGenerationToken(sync);
  }
  const outbound: TerminalOutboundCommand = {
    case: "terminalView",
    value: create(TerminalViewCommandSchema, {
      viewId: view.viewId,
      sessionId: view.session.sessionId,
      cols: intent.cols,
      rows: intent.rows,
      revision: intent.revision,
      active: intent.active,
      domainGeneration: sync.domainGeneration,
    }),
  };
  const sent = sendSyncV2Command(outbound);
  if (sent) {
    view.leaseDeadlineMs = intent.active
      ? Date.now() + TERMINAL_VIEW_LEASE_MS
      : null;
    if (intent.active && isPageVisible()) armViewAckDeadline(view, intent, sync);
    else clearViewAck(view);
    // Exact heartbeat and redial replay renew the lease without regressing an
    // accepted/baseline-ready status while its idempotent ACK is in flight.
    if (!statusCurrent) {
      emitTerminalViewStatus(view, {
        status: "pending",
        revision: intent.revision,
        active: intent.active,
      });
    }
  }
  return sent;
}

export function changeIntent(
  view: TerminalViewRecord,
  active: boolean,
  cols: number,
  rows: number,
): void {
  if (view.disposed) return;
  const current = view.desired;
  if (
    current
    && current.active === active
    && current.cols === cols
    && current.rows === rows
  ) return;
  view.rollingBack = false;
  const intent: TerminalViewIntent = {
    revision: ++view.revisionFloor,
    active,
    cols,
    rows,
  };
  view.desired = intent;
  if (!active) {
    clearViewAck(view);
    if (!hasActiveTerminalView(view.session)) {
      clearTerminalSessionLiveness(view.session, "inactive");
    }
  }
  publishIntent(view, intent);
}

export function dispatchTerminalViewState(
  frame: TerminalViewStateFrame,
  owner: TerminalGenerationToken,
): void {
  const session = terminalSessions.get(frame.sessionId);
  const view = session?.handles.get(frame.viewId);
  if (
    !session
    || !view
    || view.disposed
    || !terminalGenerationMatches(session.generation, owner)
  ) return;
  const desired = view.desired;
  if (!desired || frame.revision !== desired.revision) return;
  if (
    view.pendingViewAckRevision === desired.revision
    && terminalGenerationMatches(view.pendingViewAckGeneration, owner)
  ) {
    clearViewAck(view);
  }
  const reason = frame.reason.slice(0, 200);

  if (frame.status === TerminalViewStatus.ACCEPTED) {
    if (!frame.active) {
      if (desired.active || frame.streamId || frame.effectiveCols || frame.effectiveRows) return;
      view.rollingBack = false;
      view.accepted = { ...desired };
      view.leaseDeadlineMs = null;
      emitTerminalViewStatus(view, {
        status: "accepted",
        revision: frame.revision,
        active: false,
        streamId: "",
        effectiveCols: 0,
        effectiveRows: 0,
        baselineReady: false,
      });
      return;
    }
    if (
      !desired.active
      || !isTerminalUuid(frame.streamId)
      || !isTerminalGeometry({ cols: frame.effectiveCols, rows: frame.effectiveRows })
    ) return;
    installExpectedTerminalStream(
      session,
      frame.streamId,
      frame.effectiveCols,
      frame.effectiveRows,
    );
    view.accepted = { ...desired };
    view.rollingBack = false;
    emitTerminalViewStatus(view, {
      status: "accepted",
      revision: frame.revision,
      active: true,
      streamId: frame.streamId,
      effectiveCols: frame.effectiveCols,
      effectiveRows: frame.effectiveRows,
      baselineReady: session.baselineReady,
    });
    sendLatchedTerminalResync(session);
    if (session.baselineReady) armTerminalForegroundIdleProbe(session);
    return;
  }

  if (frame.status === TerminalViewStatus.UNAVAILABLE) {
    emitTerminalViewStatus(view, {
      status: "unavailable",
      revision: frame.revision,
      active: frame.active,
      streamId: frame.streamId,
      effectiveCols: frame.effectiveCols,
      effectiveRows: frame.effectiveRows,
      reason,
    });
    return;
  }

  if (frame.status === TerminalViewStatus.REJECTED) {
    emitTerminalViewStatus(view, {
      status: "rejected",
      revision: frame.revision,
      active: frame.active,
      streamId: frame.streamId,
      effectiveCols: frame.effectiveCols,
      effectiveRows: frame.effectiveRows,
      reason,
    });
    if (view.accepted && !view.rollingBack) {
      const rollback: TerminalViewIntent = {
        ...view.accepted,
        revision: ++view.revisionFloor,
      };
      view.rollingBack = true;
      view.desired = rollback;
      publishIntent(view, rollback);
    } else {
      view.desired = null;
    }
  }
}

function armViewAckDeadline(
  view: TerminalViewRecord,
  intent: TerminalViewIntent,
  sync: SyncV2TerminalState,
): void {
  if (
    view.pendingViewAckAtMs !== null
    && view.pendingViewAckRevision === intent.revision
    && terminalGenerationMatches(view.pendingViewAckGeneration, sync)
  ) return;
  clearViewAck(view);
  const owner = terminalGenerationToken(sync);
  const startedAt = performance.now();
  view.pendingViewAckAtMs = startedAt;
  view.pendingViewAckGeneration = owner;
  view.pendingViewAckRevision = intent.revision;
  const timer = setTimeout(() => {
    if (view.viewAckTimer !== timer) return;
    view.viewAckTimer = null;
    const current = currentSyncV2TerminalState();
    if (
      view.disposed
      || !view.desired?.active
      || view.desired.revision !== intent.revision
      || !isPageVisible()
      || !terminalGenerationMatches(view.pendingViewAckGeneration, owner)
      || !terminalGenerationMatches(owner, current)
      || view.pendingViewAckAtMs !== startedAt
    ) {
      clearViewAck(view);
      return;
    }
    signal("cell.foreground_stall", {
      sid: view.session.sessionId,
      stream_id: view.session.expectedStreamId,
      layer: "view_ack",
      action: "redial",
      age_ms: Math.max(0, performance.now() - startedAt),
      cooldownKey: view.session.sessionId,
    });
    requestSyncGenerationRecovery(owner, "terminal-view-ack-timeout");
  }, TERMINAL_VIEW_LEASE_MS);
  view.viewAckTimer = timer;
}
