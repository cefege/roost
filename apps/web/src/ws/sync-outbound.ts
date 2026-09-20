// Sync input adapter and public terminal input entry point. The document router
// owns queueing, holds, route epochs, and outcome fences; this module only
// selects the elected direct route or encodes the current Sync command. A
// terminal-domain reset is not an input fence: only socket replacement retires
// started Sync input.

import type {
  TerminalInputRouteClaim,
  TerminalInputRouteResult,
} from "@roost/shared/proto/sync_pb";
import {
  terminalDirectRegistry,
  type TerminalDirectConnection,
} from "../store/terminal-stream-transport.ts";
import { currentTerminalGrant, resetTerminalGrants } from "./local-terminal-grants.ts";
import { rootStore } from "../store/root.ts";
import type { TerminalGenerationToken } from "../store/terminal-stream-types.ts";
import {
  currentSyncV2TerminalState,
  registerSyncV2ControlHandler,
  registerSyncV2GenerationHandler,
  sendSyncV2Command,
  type SyncV2Control,
  type SyncV2TerminalState,
} from "../store/sync.ts";
import { _resetSmokeOutboundForTest } from "./sync-outbound-smoke.ts";
import { resetSyncTerminalControlProbes } from "./sync-terminal-control-probe.ts";
import type { InputAdmission } from "./terminal-input-lanes.ts";
import {
  admitTerminalInput,
  claimTerminalInputRoute,
  holdTerminalInput,
  refreshTerminalInputDestination,
  resetTerminalInputRouter,
  retireTerminalInputConnection,
  settleTerminalInput,
  terminalInputPhase,
  type TerminalInputDestination,
} from "./terminal-input-router.ts";

export {
  setSmokeTerminalInputObserver,
  setSmokeTerminalInputOutcomeObserver,
} from "./sync-outbound-smoke.ts";
export type {
  SmokeTerminalInputObserver,
  SmokeTerminalInputOutcomeObserver,
} from "./sync-outbound-smoke.ts";

type TerminalState = SyncV2TerminalState;
type OutboundCommand = Parameters<typeof sendSyncV2Command>[0];

interface SyncRouteContext {
  readonly workerEpoch: string;
  readonly inputRouteSupported: boolean;
}

interface SyncClaimWaiter {
  readonly socketId: string;
  readonly processEpoch: string;
  resolve(result: TerminalInputRouteResult): void;
  reject(error: Error): void;
  timer: ReturnType<typeof setTimeout>;
}

const syncClaimWaiters = new Map<string, SyncClaimWaiter>();
let observedSyncToken: TerminalGenerationToken | null = null;

function command(value: unknown): OutboundCommand {
  return value as OutboundCommand;
}

function syncToken(state: TerminalState, domainGeneration = state.domainGeneration): TerminalGenerationToken {
  return {
    socketGeneration: state.socketGeneration,
    socketId: state.socketId,
    processEpoch: state.processEpoch,
    domainGeneration,
    transportKind: "sync",
    workerFp: null,
  };
}

function sameSyncConnection(left: TerminalGenerationToken, right: TerminalGenerationToken): boolean {
  return left.socketGeneration === right.socketGeneration
    && left.socketId === right.socketId
    && left.processEpoch === right.processEpoch;
}

function rejectSyncClaims(reason: string, token?: TerminalGenerationToken): void {
  for (const [requestId, waiter] of syncClaimWaiters) {
    if (
      token
      && (waiter.socketId !== token.socketId || waiter.processEpoch !== token.processEpoch)
    ) continue;
    syncClaimWaiters.delete(requestId);
    clearTimeout(waiter.timer);
    waiter.reject(new Error(reason));
  }
}

function claimSyncInputRoute(
  state: TerminalState,
  routeClaim: TerminalInputRouteClaim,
): Promise<TerminalInputRouteResult> {
  const current = currentSyncV2TerminalState();
  if (
    !current?.ready
    || current.socketId !== state.socketId
    || current.processEpoch !== state.processEpoch
    || current.domainGeneration !== state.domainGeneration
  ) return Promise.reject(new Error("terminal Sync is not connected"));
  const { promise, resolve, reject } = Promise.withResolvers<TerminalInputRouteResult>();
  const timer = setTimeout(() => {
    syncClaimWaiters.delete(routeClaim.requestId);
    reject(new Error("terminal input route claim timed out"));
  }, 8_000);
  syncClaimWaiters.set(routeClaim.requestId, {
    socketId: state.socketId,
    processEpoch: state.processEpoch,
    resolve,
    reject,
    timer,
  });
  if (sendSyncV2Command(command({ case: "inputRouteClaim", value: routeClaim }))) return promise;
  syncClaimWaiters.delete(routeClaim.requestId);
  clearTimeout(timer);
  reject(new Error("terminal Sync did not accept the input route claim"));
  return promise;
}

function syncInputDestination(
  state: TerminalState,
  route: SyncRouteContext = { workerEpoch: state.processEpoch, inputRouteSupported: false },
): TerminalInputDestination {
  const routeSupported = route.inputRouteSupported;
  return {
    token: syncToken(state),
    workerEpoch: route.workerEpoch,
    inputRouteSupported: routeSupported,
    sendInput(input): "accepted" | "queued" | "refused" {
      const current = currentSyncV2TerminalState();
      if (
        !current
        || current.socketId !== state.socketId
        || current.processEpoch !== state.processEpoch
        || current.domainGeneration !== state.domainGeneration
      ) return "refused";
      if (!current.ready) return "queued";
      return sendSyncV2Command(command({ case: "input", value: input }))
        ? "accepted"
        : "refused";
    },
    ...(routeSupported ? {
      claimInputRoute: (routeClaim: TerminalInputRouteClaim) => claimSyncInputRoute(state, routeClaim),
    } : {}),
  };
}

export function terminalInputDestinationForDirectConnection(
  connection: TerminalDirectConnection,
  allowUnqualified = false,
): TerminalInputDestination | null {
  const token = connection.token();
  if (!token) return null;
  if (
    connection.kind === "webrtc"
    && !allowUnqualified
    && connection.telemetry?.().livenessQualified !== true
  ) return null;
  return {
    token,
    workerEpoch: connection.workerEpoch,
    inputRouteSupported: connection.inputRouteSupported,
    sendInput: (input) => connection.sendInput(input),
    ...(connection.inputRouteSupported ? {
      claimInputRoute: (routeClaim: TerminalInputRouteClaim) => connection.claimInputRoute(routeClaim),
    } : {}),
    close: (reason) => connection.close(reason),
  };
}

export function terminalInputDestinationForSession(sessionId: string): TerminalInputDestination | null {
  const direct = terminalDirectRegistry.activeForSession(sessionId);
  if (direct) return terminalInputDestinationForDirectConnection(direct);
  const state = currentSyncV2TerminalState();
  if (!state) return null;
  const workerFp = rootStore.sessions[sessionId]?.worker_fp;
  const grant = workerFp ? currentTerminalGrant(workerFp) : null;
  return syncInputDestination(state, {
    workerEpoch: grant?.workerEpoch || state.processEpoch,
    inputRouteSupported: grant?.inputRouteSupported === true && !!grant.workerEpoch,
  });
}
function handleControl(control: SyncV2Control, state: TerminalState): void {
  if (control.case === "inputRouteResult") {
    const waiter = syncClaimWaiters.get(control.value.requestId);
    if (
      !waiter
      || waiter.socketId !== state.socketId
      || waiter.processEpoch !== state.processEpoch
    ) return;
    syncClaimWaiters.delete(control.value.requestId);
    clearTimeout(waiter.timer);
    waiter.resolve(control.value);
    return;
  }
  switch (control.case) {
    case "inputAccepted": {
      const value = control.value;
      settleTerminalInput(syncToken(state, value.domainGeneration), {
        sessionId: value.sessionId, inputSeq: value.inputSeq, status: "accepted", writtenBytes: value.writtenBytes,
      });
      return;
    }
    case "inputRejected": {
      const value = control.value;
      settleTerminalInput(syncToken(state, value.domainGeneration), {
        sessionId: value.sessionId, inputSeq: value.inputSeq, status: "rejected", reason: value.reason,
      });
      return;
    }
    case "inputAmbiguous": {
      const value = control.value;
      settleTerminalInput(syncToken(state, value.domainGeneration), {
        sessionId: value.sessionId, inputSeq: value.inputSeq, status: "ambiguous",
        writtenBytes: value.writtenBytes, reason: value.reason,
      });
      return;
    }
    default:
      return;
  }
}

/** Refreshes only unsent input at a terminal-domain change. Started input stays
 * correlated to its original domain until control reports its real outcome. */
export function handleGeneration(state: TerminalState | null): void {
  const current = state ? syncInputDestination(state) : null;
  const currentToken = current?.token ?? null;
  if (
    observedSyncToken
    && (!currentToken || !sameSyncConnection(observedSyncToken, currentToken))
  ) {
    retireTerminalInputConnection(observedSyncToken, "Sync closed");
    rejectSyncClaims("terminal Sync closed", observedSyncToken);
  }
  if (current) refreshTerminalInputDestination(current);
  observedSyncToken = currentToken;
  if (state?.ready) void reclaimBlockedSyncInputs();
}

async function reclaimBlockedSyncInputs(): Promise<void> {
  for (const sessionId of Object.keys(rootStore.sessions)) {
    if (terminalInputPhase(sessionId) !== "blocked") continue;
    const destination = terminalInputDestinationForSession(sessionId);
    if (!destination || destination.token.transportKind !== "sync") continue;
    const release = holdTerminalInput(sessionId);
    if (!destination.inputRouteSupported) {
      release(destination);
      continue;
    }
    try {
      const claimed = await claimTerminalInputRoute(sessionId, destination);
      if (claimed.accepted) release(destination);
    } catch {
      release();
    }
  }
}

queueMicrotask(() => {
  registerSyncV2ControlHandler(handleControl);
  registerSyncV2GenerationHandler(handleGeneration);
});

/** Admit one complete PTY input batch on the elected direct route or Sync.
 * `viewId` is attribution only; callers without a mounted view omit it. */
export function sendTerminalInput(
  sessionId: string,
  bytes: Uint8Array,
  viewId?: string,
): InputAdmission {
  return admitTerminalInput(terminalInputDestinationForSession(sessionId), sessionId, bytes, viewId);
}

/** Credential teardown never replays retained bytes onto a fresh authenticated
 * transport and closes all credential-bound direct routes. */
export function resetTerminalOutboundState(reason = "credential boundary"): void {
  resetTerminalInputRouter(reason);
  rejectSyncClaims(reason);
  resetSyncTerminalControlProbes(reason);
  observedSyncToken = null;
  resetTerminalGrants();
  terminalDirectRegistry.reset(reason);
}

export function _resetTerminalOutboundForTest(): void {
  resetTerminalOutboundState("test reset");
  _resetSmokeOutboundForTest();
}
