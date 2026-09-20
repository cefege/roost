// Terminal views give UI panes stable leases over one shared session replica.
// They preserve viewport intent and renderer subscriptions while Sync generations change.
// CellTerminal creates these handles, and inbound view-state dispatch enters through this path.
// Command ordering lives beside the view while canonical cell continuity stays in the replica.

import {
  TERMINAL_VIEW_HEARTBEAT_MS,
  clampTerminalGeometry,
} from "@roost/shared/viewport";
import { markPhaseOnce } from "../lib/diag.ts";
import { isPageVisible } from "../lib/pageVisible.ts";
import { TerminalRenderScheduler } from "../lib/terminal-render-scheduler.ts";
import { subscribeTerminalBaselineProgress } from "./terminal-stream-progress.ts";
import { installTerminalTransportRetarget } from "./terminal-stream-retarget.ts";
import {
  cancelTerminalViewRenewal,
  registerTerminalViewRenewalHandler,
} from "./terminal-stream-renewal-scheduler.ts";
import {
  clearViewAck,
  changeIntent,
  hasActiveTerminalView,
  publishIntent,
} from "./terminal-stream-view-commands.ts";
import {
  clearTerminalSessionLiveness,
  deliverCanonicalToSubscriber,
  repairStaleTerminalSubscriberOnHeartbeat,
  requestTerminalDomReconcileRecovery,
} from "./terminal-stream-replica.ts";
import {
  pruneTerminalSessionState,
  terminalSessionReplica,
} from "./terminal-stream-state.ts";
import type {
  TerminalRendererForegroundPredicate,
  TerminalRendererSubscriber,
  TerminalViewHandle,
  TerminalViewRecord,
} from "./terminal-stream-types.ts";

export { dispatchTerminalViewState } from "./terminal-stream-view-commands.ts";

const alwaysForeground = (): boolean => true;

export function createTerminalView(
  sessionId: string,
  workerFp: string,
): TerminalViewHandle {
  const session = terminalSessionReplica(sessionId, workerFp);
  const view: TerminalViewRecord = {
    session,
    viewId: crypto.randomUUID(),
    revisionFloor: 0n,
    desired: null,
    accepted: null,
    status: null,
    statusListeners: new Set(),
    progressListeners: new Set(),
    lastProgressKey: null,
    rendererSubscribers: new Set(),
    rollingBack: false,
    viewAckTimer: null,
    renewalDueAtMs: null,
    leaseDeadlineMs: null,
    pendingViewAckAtMs: null,
    pendingViewAckGeneration: null,
    pendingViewAckRevision: null,
    disposed: false,
  };
  session.handles.set(view.viewId, view);

  return {
    sessionId,
    // A transport change rotates this id, so the handle reports the live one
    // instead of the identity it was created with.
    get viewId(): string { return view.viewId; },
    recoverUnreconciledDom(): void {
      requestTerminalDomReconcileRecovery(session);
    },
    setViewport(geometry): void {
      const trusted = clampTerminalGeometry(geometry);
      changeIntent(view, true, trusted.cols, trusted.rows);
      setRendererSubscribersForeground(view, true);
    },
    setInactive(): void {
      setRendererSubscribersForeground(view, false);
      changeIntent(view, false, 0, 0);
    },
    refresh(): void {
      setRendererSubscribersForeground(view, view.desired?.active === true);
      const desired = view.desired;
      if (
        !view.disposed
        && desired
        && (!desired.active || isPageVisible())
      ) publishIntent(view, desired);
    },
    subscribeStatus(listener): () => void {
      if (view.disposed) return () => undefined;
      view.statusListeners.add(listener);
      if (view.status) listener(view.status);
      return () => { view.statusListeners.delete(listener); };
    },
    subscribeProgress(listener): () => void {
      return subscribeTerminalBaselineProgress(view, listener);
    },
    subscribeRenderer(renderer, onDelivery, isForeground = alwaysForeground): () => void {
      if (view.disposed) return () => undefined;
      let subscriber: TerminalRendererSubscriber;
      const scheduler = new TerminalRenderScheduler(
        renderer,
        sessionId,
        (frame, canonical, scrollbackAppended, hadWireFull) => {
          subscriber.streamId = frame.streamId;
          subscriber.gridEpoch = frame.gridEpoch;
          subscriber.seq = frame.seq;
          markPhaseOnce("first_cell_apply", sessionId, {
            sessionId,
            sequence: frame.seq,
            full: frame.full,
          });
          subscriber.onDelivery?.({
            frame,
            full: frame.full,
            canonical,
            scrollbackAppended,
            hadWireFull,
          });
        },
      );
      subscriber = {
        sessionId,
        scheduler,
        isForeground,
        viewActive: view.desired?.active ?? true,
        onDelivery: (delivery) => {
          const desired = view.desired;
          const leaseDeadlineMs = view.leaseDeadlineMs;
          if (
            !view.disposed
            && desired?.active
            && isPageVisible()
            && leaseDeadlineMs !== null
            && leaseDeadlineMs <= Date.now() + TERMINAL_VIEW_HEARTBEAT_MS
          ) {
            publishIntent(view, desired);
          }
          onDelivery?.(delivery);
        },
        streamId: null,
        gridEpoch: null,
        seq: null,
      };
      setSubscriberForeground(subscriber, subscriber.viewActive);
      session.subscribers.add(subscriber);
      view.rendererSubscribers.add(subscriber);
      if (
        session.baselineReady
        && session.canonical?.streamId === session.expectedStreamId
      ) {
        deliverCanonicalToSubscriber(session, subscriber);
      }
      return () => {
        subscriber.scheduler.dispose();
        session.subscribers.delete(subscriber);
        view.rendererSubscribers.delete(subscriber);
      };
    },
    dispose(): void {
      if (view.disposed) return;
      if (view.desired?.active) changeIntent(view, false, 0, 0);
      clearViewAck(view);
      if (!hasActiveTerminalView(session)) {
        clearTerminalSessionLiveness(session, "disposed");
      }
      view.disposed = true;
      cancelTerminalViewRenewal(view);
      view.statusListeners.clear();
      view.lastProgressKey = null;
      view.progressListeners.clear();
      for (const subscriber of view.rendererSubscribers) {
        subscriber.scheduler.dispose();
        session.subscribers.delete(subscriber);
      }
      view.rendererSubscribers.clear();
      session.handles.delete(view.viewId);
      if (session.handles.size === 0) pruneTerminalSessionState(sessionId, session);
    },
  };
}

function renewTerminalView(view: TerminalViewRecord): void {
  const desired = view.desired;
  if (view.disposed || !desired?.active || !isPageVisible()) return;
  const session = view.session;
  const now = Date.now();
  const awaitingReplayBaseline = session.requiresFreshBaseline
    && view.status?.status === "pending"
    && view.status.revision === desired.revision
    && view.leaseDeadlineMs !== null
    && view.leaseDeadlineMs > now + TERMINAL_VIEW_HEARTBEAT_MS;
  if (awaitingReplayBaseline) return;
  const accepted = view.status?.status === "accepted"
    && view.status.revision === desired.revision;
  if (publishIntent(view, desired) && accepted) {
    repairStaleTerminalSubscriberOnHeartbeat(session);
  }
}

registerTerminalViewRenewalHandler(renewTerminalView);
queueMicrotask(installTerminalTransportRetarget);

function setRendererSubscribersForeground(
  view: TerminalViewRecord,
  active: boolean,
): void {
  for (const subscriber of view.rendererSubscribers) {
    setSubscriberForeground(subscriber, active);
  }
}

function setSubscriberForeground(
  subscriber: TerminalRendererSubscriber,
  active: boolean,
): void {
  subscriber.viewActive = active;
  subscriber.scheduler.setForeground(active && subscriber.isForeground());
}
