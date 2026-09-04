// This is the stable public entry point for the browser's single Sync socket.
// The dial loop stays here so socket identity and callback ordering remain obvious.
// Cohesive leaves own domain hydration, inbound framing, link state, and redial policy.
// Existing callers keep importing this module rather than its private collaboration seams.

import { SyncDomain, type FirehoseFrame } from "@roost/shared/proto/sync_pb";
import { diag, signal } from "@roost/shared/diag";
import {
  SYNC_AUTH_SUBPROTOCOL,
  SYNC_QUERY_FLOW_V1,
  SYNC_QUERY_V2,
  SYNC_WS_PATH,
} from "@roost/shared/wire/sync-ws";
import { signCoordinatorJwt } from "../auth/web-key.ts";
import { getTabId } from "../auth/tab-id.ts";
import { coordinatorBaseUrl } from "../connect.ts";
import { selectedDashboardId } from "./root.ts";
import {
  canAcceptSyncLink,
  canOpenSyncLink,
  decodeFirehoseFrame,
  isSyncBackpressureClose,
} from "./sync-flow.ts";
import { lastSeenSyncEventId } from "./sync-frame.ts";
import { _consumeSyncFrame } from "./sync-inbound.ts";
import {
  _allocateSyncSocketGeneration,
  _armSyncCloseEscape,
  _clearLiveSyncLink,
  _cleanupSyncLink,
  _closeFailedSyncLink,
  _currentLiveSyncLink,
  _deactivateSyncV2Link,
  _installLiveSyncLink,
  _initiateSyncClose,
  type LiveSyncLink,
  type SyncAbortReason,
} from "./sync-link-state.ts";
import {
  _noteSyncV2FrameReceived,
  _waitForNextSyncDial,
  _waitForSyncDialPermission,
  resumeSyncNow,
  _syncDialAttempt,
} from "./sync-redial.ts";
import {
  startStaleWatchdog,
  SYNC_OPEN_TIMEOUT_MS,
} from "./sync-watchdog.ts";
import type { TerminalGenerationToken } from "./terminal-stream-types.ts";

export { workerOnline } from "./sync-routable.ts";
export { registerPresenceHandler } from "./sync-dispatch.ts";
export {
  cellFrameCount,
  cellFullFrameCount,
  lastFullFrameSbRows,
  cellGridEpoch,
} from "./terminal-stream.ts";
export { registerAuditDelta } from "./sync-handlers.ts";
export { resetLastSeenSyncEventId } from "./sync-frame.ts";
export {
  forceSyncMaxBackoff,
  forceSyncReconnect,
  pauseSyncTransport,
  resumeSyncTransport,
} from "./sync-smoke.ts";
export {
  currentSyncDomainToken,
  currentSyncV2TerminalState,
  isCurrentSyncDomainToken,
  registerSyncV2GenerationHandler,
  syncWsGeneration,
  type SyncDomainToken,
  type SyncV2TerminalState,
} from "./sync-link-state.ts";
export {
  applySyncDomainSnapshot,
  registerSyncV2ControlHandler,
  sendSyncV2Command,
  waitForSyncSubscribed,
  type SyncDomainHydrator,
  type SyncDomainSnapshot,
  type SyncSubscribedState,
  type SyncV2Control,
} from "./sync-domain-state.ts";
export {
  registerLazySyncDomain,
  registerSyncDomainHydrator,
} from "./sync-domain-hydration.ts";
export {
  _armSyncRedialFloor,
  _setSmokeTransportPaused,
  installSyncLifecycleWake,
  reconnectNow,
  resumeSyncNow,
  syncRedialStatus,
  type SyncRedialStatus,
} from "./sync-redial.ts";

const SYNC_AUTH_REVOKED_CLOSE_CODE = 4001;
let syncAuthRejected: (() => void) | null = null;
let tenantRouteSwitchSuspended = false;
const TENANT_ROUTE_SWITCH_HOLD = new Promise<void>(() => {
  // A route-bound transport may resume only in the freshly loaded document.
});
interface DashboardSwitchHold {
  promise: Promise<void>;
  resolve: () => void;
}

let dashboardSwitchHold: DashboardSwitchHold | null = null;

async function waitForScopeDialPermission(): Promise<void> {
  while (true) {
    if (tenantRouteSwitchSuspended) await TENANT_ROUTE_SWITCH_HOLD;
    const currentHold = dashboardSwitchHold;
    if (!currentHold) return;
    await currentHold.promise;
  }
}

export type TerminalGenerationRecoveryReason =
  | "terminal-view-ack-timeout"
  | "terminal-proof-timeout";

export function buildSyncWebSocketUrl(
  httpBase: string,
  sinceEventId: number,
  tabId: string,
  dashboardId: string | null,
): string {
  const wsBase = httpBase.replace(/^http/, "ws");
  const dashboardQuery = dashboardId
    ? `&dashboard=${encodeURIComponent(dashboardId)}`
    : "";
  return `${wsBase}${SYNC_WS_PATH}?since=${sinceEventId}&tab=${encodeURIComponent(tabId)}&flow=${SYNC_QUERY_FLOW_V1}&sync_v=${SYNC_QUERY_V2}${dashboardQuery}`;
}

export function registerSyncAuthRejectionHandler(handler: () => void): () => void {
  syncAuthRejected = handler;
  return () => {
    if (syncAuthRejected === handler) syncAuthRejected = null;
  };
}

/** Permanently retire this document's route-bound Sync transport. */
export function suspendSyncForTenantRouteSwitch(): void {
  if (tenantRouteSwitchSuspended) return;
  tenantRouteSwitchSuspended = true;
  _initiateSyncClose("manual");
}

/** Close the live tube intentionally so the existing loop immediately redials. */
export function _requestSyncRedial(): void {
  _initiateSyncClose("manual");
}

/** Stop the live tube and prevent another scope from dialing until the exact
 * dashboard-selection attempt either commits or restores its predecessor. */
export function holdSyncForDashboardSwitch(): void {
  dashboardSwitchHold ??= Promise.withResolvers<void>();
  _initiateSyncClose("manual");
}

export function releaseSyncAfterDashboardSwitch(): void {
  const currentHold = dashboardSwitchHold;
  if (!currentHold) return;
  dashboardSwitchHold = null;
  currentHold.resolve();
  resumeSyncNow();
}

/** Test/diagnostic seam for the switch transaction's no-dial invariant. */
export function _syncDashboardSwitchHeld(): boolean {
  return dashboardSwitchHold !== null;
}

/** Make terminal commands inert without retaining a resumable switch hold. */
export function closeSyncForDashboardSwitch(): void {
  _initiateSyncClose("manual");
}

export function requestSyncGenerationRecovery(
  expected: TerminalGenerationToken,
  reason: TerminalGenerationRecoveryReason,
): boolean {
  const link = _currentLiveSyncLink();
  const terminal = link?.v2?.domains.get(SyncDomain.TERMINAL);
  if (
    !link
    || !link.accepting
    || link.abortReason !== null
    || link.ws.readyState !== WebSocket.OPEN
    || link.gen !== expected.socketGeneration
    || link.v2?.socketId !== expected.socketId
    || link.v2.processEpoch !== expected.processEpoch
    || terminal?.generation !== expected.domainGeneration
  ) return false;

  link.accepting = false;
  link.abortReason = "terminal-liveness";
  _deactivateSyncV2Link(link);
  diag("sync.terminal_liveness_recovery", {
    reason,
    socket_generation: expected.socketGeneration,
    socket_id: expected.socketId,
    process_epoch: expected.processEpoch,
    domain_generation: expected.domainGeneration.toString(),
  });
  try {
    link.ws.close(4000, "terminal liveness timeout");
  } catch {
    link.resolveClosed();
  }
  _armSyncCloseEscape(link);
  resumeSyncNow();
  return true;
}

// Raw WebSocket transport avoids Bun's long-lived RequestContext abort crash.
// FirehoseFrame protobuf bytes and causal ACK semantics remain unchanged.
export async function _runConnectSync(): Promise<void> {
  while (true) {
    await waitForScopeDialPermission();
    const permissionWait = _waitForSyncDialPermission();
    if (permissionWait) await permissionWait;
    await waitForScopeDialPermission();
    let dialLink: LiveSyncLink | null = null;
    let abortReason: SyncAbortReason = null;
    try {
      console.debug("[sync.connect] starting Sync stream", {
        sinceEventId: lastSeenSyncEventId(),
        attempt: _syncDialAttempt(),
      });
      const coordinatorBase = coordinatorBaseUrl();
      const jwt = await signCoordinatorJwt();
      // The JWT stays in a subprotocol, never the URL. Capture scope-bound
      // query values after the final hold; no await may split this from opening.
      await waitForScopeDialPermission();
      const url = buildSyncWebSocketUrl(
        coordinatorBase,
        lastSeenSyncEventId(),
        getTabId(),
        selectedDashboardId(),
      );
      const ws = new WebSocket(url, [SYNC_AUTH_SUBPROTOCOL, jwt]);
      ws.binaryType = "arraybuffer";
      const gen = _allocateSyncSocketGeneration();
      const { promise: closed, resolve: resolveClosed } = Promise.withResolvers<void>();
      const link: LiveSyncLink = {
        ws,
        gen,
        abortReason: null,
        accepting: false,
        resolveClosed,
        expectsV2: true,
        openTimer: null,
        closeEscapeTimer: null,
        watchdog: null,
        v2: null,
      };
      dialLink = link;
      _installLiveSyncLink(link);
      link.openTimer = setTimeout(() => {
        if (
          _currentLiveSyncLink() !== link
          || link.gen !== gen
          || link.abortReason !== null
          || ws.readyState !== WebSocket.CONNECTING
        ) return;
        link.openTimer = null;
        _closeFailedSyncLink(link);
      }, SYNC_OPEN_TIMEOUT_MS);
      ws.onopen = () => {
        clearTimeout(link.openTimer ?? undefined);
        link.openTimer = null;
        if (!canOpenSyncLink(_currentLiveSyncLink(), link, WebSocket.OPEN)) {
          link.accepting = false;
          try { ws.close(); } catch { /* obsolete or closing dial */ }
          return;
        }
        link.accepting = true;
        // OPEN is not hydration readiness; subscribed installs generations.
        link.watchdog = startStaleWatchdog(ws, {
          onStale: () => {
            if (_currentLiveSyncLink() === link) _initiateSyncClose("stale");
          },
        });
      };
      ws.onmessage = (event) => {
        if (!canAcceptSyncLink(_currentLiveSyncLink(), link, WebSocket.OPEN)) return;
        let frame: FirehoseFrame;
        try {
          frame = decodeFirehoseFrame(new Uint8Array(event.data as ArrayBuffer));
        } catch (error) {
          signal("diag.corruption_signal", {
            kind: "sync_ws_decode",
            frame: "firehose",
            msg: String(error),
            cooldownKey: "sync",
          });
          _closeFailedSyncLink(link);
          return;
        }
        _consumeSyncFrame(link, frame);
        if (link.v2) _noteSyncV2FrameReceived();
      };
      ws.onerror = (error) => { console.debug("[sync.connect] ws error", error); };
      ws.onclose = (event) => {
        if (
          event.code === SYNC_AUTH_REVOKED_CLOSE_CODE
          && _currentLiveSyncLink() === link
          && link.abortReason === null
        ) syncAuthRejected?.();
        link.accepting = false;
        if (
          link.abortReason === null
          && isSyncBackpressureClose(event.code, event.reason)
        ) link.abortReason = "flow";
        link.resolveClosed();
      };
      await closed;
      console.debug("[sync.connect] stream ended; re-dialing");
    } catch (error) {
      console.debug("[sync.connect] stream error; backing off", error);
    } finally {
      if (dialLink) {
        abortReason = dialLink.abortReason;
        _cleanupSyncLink(dialLink);
        _clearLiveSyncLink(dialLink);
      }
    }

    const redialWait = _waitForNextSyncDial(abortReason);
    if (redialWait) await redialWait;
  }
}
