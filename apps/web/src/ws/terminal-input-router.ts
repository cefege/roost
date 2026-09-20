// Document-owned terminal input routing. Every transport presents an exact
// generation token and only encodes accepted commands; this owner retains
// bounded batches, transition holds, route claims, and result correlation.
// Session cleanup and credential teardown call this owner before a replacement
// transport can receive any byte retained by an earlier route.

import { create } from "@bufbuild/protobuf";
import {
  InputCommandSchema,
  type InputCommand,
  type TerminalInputRouteClaim,
  type TerminalInputRouteResult,
} from "@roost/shared/proto/sync_pb";
import type { TerminalGenerationToken } from "../store/terminal-stream-types.ts";
import {
  createTerminalInputLanes,
  forgetTerminalInputSendTs,
  resetTerminalInputSendTs,
  type InputAdmission,
  type InputOutcome,
  type PendingTerminalInput,
} from "./terminal-input-lanes.ts";
import {
  claimTerminalInputRouteState, refreshTerminalInputRouteState,
  type TerminalInputPhase,
  type TerminalInputRouteClaimOutcome,
  type TerminalInputRouteClaimState,
} from "./terminal-input-route-claim.ts";
export { MAX_TERMINAL_INPUT_ROUTE_REVISION } from "./terminal-input-route-claim.ts";
export const HELD_INPUT_ADMISSION_TIMEOUT_MS = 10_000;
export type { TerminalInputPhase, TerminalInputRouteClaimOutcome };
export type TerminalInputSendStatus = "accepted" | "queued" | "refused";
export interface TerminalInputDestination {
  readonly token: TerminalGenerationToken;
  readonly workerEpoch: string;
  /** Old loopback workers do not understand route claims and retain their
   * existing fresh-baseline input behavior. */
  readonly inputRouteSupported: boolean;
  sendInput(command: InputCommand): TerminalInputSendStatus;
  claimInputRoute?(command: TerminalInputRouteClaim): Promise<TerminalInputRouteResult>;
  close?(reason: string): void;
}

export interface TerminalInputSettlement {
  readonly sessionId: string;
  readonly inputSeq: bigint;
  readonly status: InputOutcome["status"];
  readonly writtenBytes?: number;
  readonly reason?: string;
}

/** A release without an acknowledged destination rejects held input; it never
 * reuses the route that was active when the hold began. */
export type TerminalInputHoldRelease = (destination?: TerminalInputDestination) => void;

interface TerminalInputFence {
  readonly destination: TerminalInputDestination;
  readonly tokenKey: string;
  readonly connectionKey: string;
}

type LaneFence = TerminalInputFence | null;

interface SessionInputState extends TerminalInputRouteClaimState {
  holdId: number;
}

export interface TerminalInputPendingSnapshot { readonly count: number; }

export interface TerminalInputRouter {
  admit(destination: TerminalInputDestination | null, sessionId: string, bytes: Uint8Array, viewId?: string): InputAdmission;
  hold(sessionId: string): TerminalInputHoldRelease;
  drain(sessionId: string, token: TerminalGenerationToken): Promise<void>;
  claim(sessionId: string, destination: TerminalInputDestination): Promise<TerminalInputRouteClaimOutcome>;
  refresh(destination: TerminalInputDestination): void;
  retire(token: TerminalGenerationToken, reason: string): void;
  retireConnection(token: TerminalGenerationToken, reason: string): void;
  settle(token: TerminalGenerationToken, settlement: TerminalInputSettlement): void;
  prune(sessionId: string): void;
  reset(reason: string): void;
  dispose(reason?: string): void;
  phase(sessionId: string): TerminalInputPhase | null; pendingSnapshot(sessionId: string): TerminalInputPendingSnapshot;
}
function tokenKey(token: TerminalGenerationToken): string {
  return JSON.stringify([
    token.socketGeneration,
    token.socketId,
    token.processEpoch,
    token.domainGeneration.toString(),
    token.transportKind,
    token.workerFp,
  ]);
}

function connectionKey(token: TerminalGenerationToken): string {
  return JSON.stringify([
    token.socketGeneration,
    token.socketId,
    token.processEpoch,
    token.transportKind,
    token.workerFp,
  ]);
}
export function createTerminalInputRouter(revisions = new Map<string, bigint>()): TerminalInputRouter {
  const sessions = new Map<string, SessionInputState>();
  const lanes = createTerminalInputLanes<LaneFence>((sessionId, outcome) => {
    const state = sessions.get(sessionId);
    if (!state) return;
    if (outcome.status === "ambiguous") state.phase = "ambiguous";
  });

  function stateFor(sessionId: string): SessionInputState {
    let state = sessions.get(sessionId);
    if (!state) {
      state = {
        phase: "sending",
        holdId: 0,
        claimId: 0,
        routeEpoch: "",
        routeTokenKey: null,
        activeDestination: null,
      };
      sessions.set(sessionId, state);
    }
    return state;
  }

  function fenceFor(destination: TerminalInputDestination): TerminalInputFence {
    return {
      destination,
      tokenKey: tokenKey(destination.token),
      connectionKey: connectionKey(destination.token),
    };
  }

  function finishRejected(
    pending: PendingTerminalInput<LaneFence>,
    reason: string,
  ): void {
    lanes.finish(pending, {
      status: "rejected",
      inputSeq: pending.inputSeq,
      writtenBytes: 0,
      reason,
    });
  }

  function dispatchPending(
    state: SessionInputState,
    pending: PendingTerminalInput<LaneFence>,
    fence: TerminalInputFence,
  ): void {
    if (pending.started) return;
    const inputRouteEpoch = state.routeTokenKey === fence.tokenKey
      ? state.routeEpoch
      : "";
    let status: TerminalInputSendStatus;
    try {
      status = fence.destination.sendInput(create(InputCommandSchema, {
        sessionId: pending.sessionId,
        inputSeq: pending.inputSeq,
        data: pending.bytes,
        domainGeneration: fence.destination.token.domainGeneration,
        inputRouteEpoch,
        ...(pending.viewId === undefined ? {} : { viewId: pending.viewId }),
      }));
    } catch {
      finishRejected(pending, "terminal transport did not accept input");
      return;
    }
    if (status === "accepted") {
      lanes.markStarted(pending);
      return;
    }
    if (status === "refused") finishRejected(pending, "terminal transport did not accept input");
  }

  function armHeldAdmission(
    pending: PendingTerminalInput<LaneFence>,
  ): void {
    pending.timer = setTimeout(() => {
      finishRejected(pending, "terminal input route is reconnecting");
    }, HELD_INPUT_ADMISSION_TIMEOUT_MS);
  }

  function finishForRetirement(
    pending: PendingTerminalInput<LaneFence>,
    reason: string,
  ): void {
    if (!pending.started) {
      finishRejected(pending, `${reason} before input was sent`);
      return;
    }
    lanes.finish(pending, {
      status: "ambiguous",
      inputSeq: pending.inputSeq,
      writtenBytes: 0,
      reason: `${reason} after input was sent; the batch will not be retried`,
    });
  }

  async function claim(
    sessionId: string,
    destination: TerminalInputDestination,
  ): Promise<TerminalInputRouteClaimOutcome> {
    return claimTerminalInputRouteState(
      stateFor(sessionId),
      revisions,
      sessionId,
      destination,
      tokenKey(destination.token),
    );
  }

  const router: TerminalInputRouter = {
    admit(destination, sessionId, bytes, viewId): InputAdmission {
      const refusal = lanes.refuse(sessionId, bytes.byteLength);
      if (refusal) return { accepted: false, reason: refusal };
      const state = stateFor(sessionId);
      if (state.phase === "closed") return { accepted: false, reason: "terminal session is closed" };
      if (state.phase === "blocked" || state.phase === "ambiguous") return { accepted: false, reason: "terminal input route is reconnecting" };
      if (state.phase === "holding" || state.phase === "claiming") {
        const admitted = lanes.enqueue(sessionId, bytes, viewId, null);
        armHeldAdmission(admitted.pending);
        return { accepted: true, inputSeq: admitted.inputSeq, result: admitted.result };
      }
      if (!destination) return { accepted: false, reason: "terminal transport is not connected" };
      const fence = fenceFor(destination);
      state.activeDestination = destination;
      const admitted = lanes.enqueue(sessionId, bytes, viewId, fence);
      dispatchPending(state, admitted.pending, fence);
      return { accepted: true, inputSeq: admitted.inputSeq, result: admitted.result };
    },
    hold(sessionId): TerminalInputHoldRelease {
      const state = stateFor(sessionId);
      if (state.phase === "closed") return () => undefined;
      const holdId = ++state.holdId;
      state.phase = "holding";
      return (destination): void => {
        if (state.holdId !== holdId || state.phase === "closed" || state.phase === "blocked") return;
        if (state.phase === "claiming") return;
        if (!destination) {
          for (const pending of lanes.pending()) {
            if (pending.sessionId === sessionId && pending.fence === null) {
              finishRejected(pending, "terminal transport is not connected");
            }
          }
          return;
        }
        state.activeDestination = destination;
        state.phase = "sending";
        const fence = fenceFor(destination);
        for (const pending of lanes.pending()) {
          if (pending.sessionId !== sessionId || pending.fence !== null) continue;
          pending.fence = fence;
          dispatchPending(state, pending, fence);
        }
      };
    },
    async drain(sessionId, token): Promise<void> {
      const expected = tokenKey(token);
      const outcomes = await Promise.all(lanes.pending()
        .filter((pending) => pending.sessionId === sessionId && pending.started && pending.fence?.tokenKey === expected)
        .map((pending) => pending.result));
      if (outcomes.some((outcome) => outcome.status === "ambiguous")) {
        throw new Error("terminal input route cannot drain an ambiguous batch");
      }
    },
    claim,
    refresh(destination): void {
      const current = fenceFor(destination); refreshTerminalInputRouteState(sessions.values(), destination, current.tokenKey, current.connectionKey, tokenKey, connectionKey);
      for (const pending of lanes.pending()) {
        const previous = pending.fence;
        if (!previous || previous.connectionKey !== current.connectionKey || pending.started) continue;
        if (previous.tokenKey !== current.tokenKey) {
          finishRejected(pending, "terminal generation closed before input was sent");
          continue;
        }
        pending.fence = current;
        dispatchPending(stateFor(pending.sessionId), pending, current);
      }
    },
    retire(token, reason): void {
      const expected = tokenKey(token);
      for (const pending of lanes.pending()) {
        if (pending.fence?.tokenKey === expected) finishForRetirement(pending, reason);
      }
      for (const state of sessions.values()) {
        if (state.activeDestination && tokenKey(state.activeDestination.token) === expected && state.phase !== "ambiguous") state.phase = "blocked";
      }
    },
    retireConnection(token, reason): void {
      const expected = connectionKey(token);
      for (const pending of lanes.pending()) {
        if (pending.fence?.connectionKey === expected) finishForRetirement(pending, reason);
      }
      for (const state of sessions.values()) {
        if (state.activeDestination && connectionKey(state.activeDestination.token) === expected && state.phase !== "ambiguous") state.phase = "blocked";
      }
    },
    settle(token, settlement): void {
      const pending = lanes.find(settlement.sessionId, settlement.inputSeq);
      if (!pending || !pending.started || pending.fence?.tokenKey !== tokenKey(token)) return;
      if (settlement.status === "accepted") {
        const writtenBytes = settlement.writtenBytes ?? 0;
        if (writtenBytes === pending.bytes.byteLength) {
          lanes.finish(pending, { status: "accepted", inputSeq: pending.inputSeq, writtenBytes });
        } else {
          lanes.finish(pending, {
            status: "ambiguous",
            inputSeq: pending.inputSeq,
            writtenBytes,
            reason: "terminal accepted an incomplete input batch",
          });
        }
        return;
      }
      if (settlement.status === "rejected") {
        finishRejected(pending, settlement.reason ?? "terminal rejected the input batch");
        return;
      }
      lanes.finish(pending, {
        status: "ambiguous",
        inputSeq: pending.inputSeq,
        writtenBytes: settlement.writtenBytes ?? 0,
        reason: settlement.reason ?? "terminal could not confirm the input batch",
      });
    },
    prune(sessionId): void {
      forgetTerminalInputSendTs(sessionId);
      lanes.prune(sessionId, "session closed");
      const state = stateFor(sessionId);
      state.phase = "closed";
      state.routeEpoch = "";
      state.routeTokenKey = null;
      state.activeDestination = null;
      state.claimId += 1;
    },
    reset(reason): void {
      lanes.clear(reason);
      sessions.clear();
      resetTerminalInputSendTs();
    },
    dispose(reason): void {
      lanes.dispose(reason);
      sessions.clear();
      resetTerminalInputSendTs();
    },
    phase(sessionId): TerminalInputPhase | null {
      return sessions.get(sessionId)?.phase ?? null;
    },
    pendingSnapshot(sessionId): TerminalInputPendingSnapshot {
      return { count: lanes.pending().filter((pending) => pending.sessionId === sessionId).length };
    },
  };
  return router;
}

const documentTerminalInputRouter = createTerminalInputRouter();

export function admitTerminalInput(
  destination: TerminalInputDestination | null, sessionId: string, bytes: Uint8Array, viewId?: string,
): InputAdmission { return documentTerminalInputRouter.admit(destination, sessionId, bytes, viewId); }

export function holdTerminalInput(sessionId: string): TerminalInputHoldRelease {
  return documentTerminalInputRouter.hold(sessionId);
}

export function drainTerminalInput(sessionId: string, token: TerminalGenerationToken): Promise<void> {
  return documentTerminalInputRouter.drain(sessionId, token);
}

export function claimTerminalInputRoute(
  sessionId: string, destination: TerminalInputDestination,
): Promise<TerminalInputRouteClaimOutcome> {
  return documentTerminalInputRouter.claim(sessionId, destination);
}

export function refreshTerminalInputDestination(destination: TerminalInputDestination): void {
  documentTerminalInputRouter.refresh(destination);
}

export function retireTerminalInput(token: TerminalGenerationToken, reason: string): void {
  documentTerminalInputRouter.retire(token, reason);
}

export function retireTerminalInputConnection(token: TerminalGenerationToken, reason: string): void {
  documentTerminalInputRouter.retireConnection(token, reason);
}

export function settleTerminalInput(token: TerminalGenerationToken, settlement: TerminalInputSettlement): void {
  documentTerminalInputRouter.settle(token, settlement);
}

export function pruneTerminalInputRoute(sessionId: string): void { documentTerminalInputRouter.prune(sessionId); }
export function resetTerminalInputRouter(reason: string): void { documentTerminalInputRouter.reset(reason); }
export function terminalInputPendingSnapshot(sessionId: string): TerminalInputPendingSnapshot { return documentTerminalInputRouter.pendingSnapshot(sessionId); }
export function terminalInputPhase(sessionId: string): TerminalInputPhase | null { return documentTerminalInputRouter.phase(sessionId); }
export function _terminalInputRouterForTest(): TerminalInputRouter { return documentTerminalInputRouter; }
