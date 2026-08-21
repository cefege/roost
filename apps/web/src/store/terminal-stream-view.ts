import { create } from "@bufbuild/protobuf";
import { cloneCellGridFrame } from "@roost/shared/cell";
import {
  TerminalViewCommandSchema,
  TerminalViewStatus,
  type TerminalViewStateFrame,
} from "@roost/shared/proto/sync_pb";
import {
  TERMINAL_VIEW_HEARTBEAT_MS,
  TERMINAL_VIEW_LEASE_MS,
  clampTerminalGeometry,
  isTerminalGeometry,
  isTerminalUuid,
} from "@roost/shared/viewport";
import {
  currentSyncV2TerminalState,
  registerSyncV2GenerationHandler,
  sendSyncV2Command,
  type SyncV2TerminalState,
} from "./sync.ts";
import { clearTerminalChunkTransfer } from "./terminal-stream-chunks.ts";
import {
  applyTerminalFrameToSubscriber,
  deliverCanonicalToSubscriber,
  installExpectedTerminalStream,
  repairStaleTerminalSubscriberOnHeartbeat,
  sendLatchedTerminalResync,
  terminalGenerationKey,
} from "./terminal-stream-replica.ts";
import {
  emitTerminalViewStatus,
  pruneTerminalSessionState,
  terminalGenerationObservation,
  terminalSessionReplica,
  terminalSessions,
} from "./terminal-stream-state.ts";
import type {
  TerminalOutboundCommand,
  TerminalRendererSubscriber,
  TerminalViewHandle,
  TerminalViewIntent,
  TerminalViewRecord,
} from "./terminal-stream-types.ts";

function publishIntent(
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

function changeIntent(
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
  publishIntent(view, intent);
}

export function dispatchTerminalViewState(frame: TerminalViewStateFrame): void {
  const session = terminalSessions.get(frame.sessionId);
  const view = session?.handles.get(frame.viewId);
  if (!session || !view || view.disposed) return;
  const desired = view.desired;
  if (!desired || frame.revision !== desired.revision) return;
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

function handleGeneration(state: SyncV2TerminalState | null): void {
  const nextGenerationKey = state ? terminalGenerationKey(state) : null;
  const generationChanged = terminalGenerationObservation.initialized
    && terminalGenerationObservation.key !== nextGenerationKey;
  terminalGenerationObservation.initialized = true;
  terminalGenerationObservation.key = nextGenerationKey;
  for (const session of terminalSessions.values()) {
    clearTerminalChunkTransfer(session);
    session.resyncSentGeneration = null;
    if (generationChanged && state !== null) {
      session.requiresFreshBaseline = true;
      session.baselineReady = false;
    }
    for (const view of session.handles.values()) {
      if (view.disposed || !view.desired) continue;
      if (
        generationChanged
        && (
          view.status?.status !== "pending"
          || view.status.revision !== view.desired.revision
        )
      ) {
        emitTerminalViewStatus(view, {
          status: "pending",
          revision: view.desired.revision,
          active: view.desired.active,
        });
      }
      publishIntent(view, view.desired, state);
    }
    if (state?.ready) sendLatchedTerminalResync(session);
  }
}

queueMicrotask(() => registerSyncV2GenerationHandler(handleGeneration));

export function createTerminalView(sessionId: string): TerminalViewHandle {
  const session = terminalSessionReplica(sessionId);
  const viewId = crypto.randomUUID();
  const view: TerminalViewRecord = {
    session,
    viewId,
    revisionFloor: 0n,
    desired: null,
    accepted: null,
    status: null,
    statusListeners: new Set(),
    rendererSubscribers: new Set(),
    rollingBack: false,
    heartbeat: 0 as unknown as ReturnType<typeof setInterval>,
    leaseDeadlineMs: null,
    disposed: false,
  };
  session.handles.set(viewId, view);
  view.heartbeat = setInterval(() => {
    const desired = view.desired;
    if (
      !view.disposed
      && desired?.active
      && publishIntent(view, desired)
    ) repairStaleTerminalSubscriberOnHeartbeat(session);
  }, TERMINAL_VIEW_HEARTBEAT_MS);

  return {
    sessionId,
    viewId,
    setViewport(geometry): void {
      const trusted = clampTerminalGeometry(geometry);
      changeIntent(view, true, trusted.cols, trusted.rows);
    },
    setInactive(): void {
      changeIntent(view, false, 0, 0);
    },
    refresh(): void {
      if (!view.disposed && view.desired) publishIntent(view, view.desired);
    },
    subscribeStatus(listener): () => void {
      if (view.disposed) return () => undefined;
      view.statusListeners.add(listener);
      if (view.status) listener(view.status);
      return () => { view.statusListeners.delete(listener); };
    },
    subscribeRenderer(renderer, onDelivery): () => void {
      if (view.disposed) return () => undefined;
      const subscriber: TerminalRendererSubscriber = {
        renderer,
        onDelivery,
        streamId: null,
        gridEpoch: null,
        seq: null,
      };
      session.subscribers.add(subscriber);
      view.rendererSubscribers.add(subscriber);
      if (
        session.baselineReady
        && session.canonical?.streamId === session.expectedStreamId
      ) {
        deliverCanonicalToSubscriber(session, subscriber);
      }
      return () => {
        session.subscribers.delete(subscriber);
        view.rendererSubscribers.delete(subscriber);
      };
    },
    dispose(): void {
      if (view.disposed) return;
      if (view.desired?.active) changeIntent(view, false, 0, 0);
      view.disposed = true;
      clearInterval(view.heartbeat);
      view.statusListeners.clear();
      for (const subscriber of view.rendererSubscribers) {
        session.subscribers.delete(subscriber);
      }
      view.rendererSubscribers.clear();
      session.handles.delete(viewId);
      if (session.handles.size === 0) pruneTerminalSessionState(sessionId);
    },
  };
}
