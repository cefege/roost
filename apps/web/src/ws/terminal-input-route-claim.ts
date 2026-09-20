// Input-route claim sequencing and current-aware transition holds for the
// document terminal input router. The router supplies lane release hooks so
// this owner never retains queues or transport callbacks. Claims validate
// exact replies and install only the acknowledged route epoch.

import { create } from "@bufbuild/protobuf";
import {
  TerminalInputRouteClaimSchema,
  type TerminalInputRouteResult,
} from "@roost/shared/proto/sync_pb";
import type { TerminalGenerationToken } from "../store/terminal-stream-types.ts";
import type { TerminalInputDestination } from "./terminal-input-router.ts";

export const MAX_TERMINAL_INPUT_ROUTE_REVISION = (1n << 63n) - 1n;

export type TerminalInputPhase = "sending" | "holding" | "claiming" | "blocked" | "closed";

export type TerminalInputRouteClaimOutcome =
  | { accepted: true; inputRouteEpoch: string; revision: bigint }
  | { accepted: false; unsupported: true; reason: string }
  | { accepted: false; unsupported: false; reason: string };

export interface TerminalInputRouteClaimState {
  phase: TerminalInputPhase;
  holdId: number;
  claimId: number;
  requiresRouteClaim: boolean;
  routeEpoch: string;
  routeTokenKey: string | null;
  activeDestination: TerminalInputDestination | null;
}

export interface TerminalInputHold {
  isCurrent(): boolean;
  release(destination?: TerminalInputDestination): void;
}

export interface TerminalInputHoldHooks {
  release(destination: TerminalInputDestination): void;
  rejectUnsent(reason: string): void;
  tokenKey(token: TerminalGenerationToken): string;
}

export function createTerminalInputHold(
  state: TerminalInputRouteClaimState,
  hooks: TerminalInputHoldHooks,
): TerminalInputHold {
  if (state.phase === "closed") return { isCurrent: () => false, release: () => undefined };
  const holdId = ++state.holdId;
  state.claimId += 1;
  state.phase = "holding";
  const isCurrent = (): boolean => state.holdId === holdId && state.phase !== "closed";
  return {
    isCurrent,
    release(destination): void {
      if (!isCurrent()) return;
      if (!destination) {
        state.phase = "blocked";
        state.claimId += 1;
        state.holdId += 1;
        hooks.rejectUnsent("terminal transport is not connected");
        return;
      }
      const destinationTokenKey = hooks.tokenKey(destination.token);
      if (
        state.requiresRouteClaim
        && (state.routeEpoch === "" || state.routeTokenKey !== destinationTokenKey)
      ) return;
      state.activeDestination = destination;
      state.phase = "sending";
      state.claimId += 1;
      state.holdId += 1;
      hooks.release(destination);
    },
  };
}

export function createTerminalRequestId(): string {
  const randomUuid = globalThis.crypto?.randomUUID;
  if (!randomUuid) throw new Error("browser does not provide UUID request identifiers");
  return randomUuid.call(globalThis.crypto);
}

async function awaitRouteClaim(
  claim: Promise<TerminalInputRouteResult>,
): Promise<TerminalInputRouteResult> {
  const { promise: deadline, reject } = Promise.withResolvers<TerminalInputRouteResult>();
  const timer = setTimeout(() => reject(new Error("terminal input route claim timed out")), 8_000);
  try {
    return await Promise.race([claim, deadline]);
  } finally {
    clearTimeout(timer);
  }
}

export async function claimTerminalInputRouteState(
  state: TerminalInputRouteClaimState,
  revisions: Map<string, bigint>,
  sessionId: string,
  destination: TerminalInputDestination,
  destinationTokenKey: string,
): Promise<TerminalInputRouteClaimOutcome> {
  if (!destination.inputRouteSupported || !destination.claimInputRoute) {
    return { accepted: false, unsupported: true, reason: "terminal input routes are unsupported" };
  }
  state.requiresRouteClaim = true;
  state.routeEpoch = "";
  state.routeTokenKey = null;
  const previous = revisions.get(sessionId) ?? 0n;
  if (previous >= MAX_TERMINAL_INPUT_ROUTE_REVISION) {
    state.phase = "blocked";
    destination.close?.("terminal input route revision exhausted");
    return { accepted: false, unsupported: false, reason: "terminal input route revision exhausted" };
  }
  const claimId = ++state.claimId;
  state.phase = "claiming";
  state.activeDestination = destination;
  let revision = previous + 1n;
  revisions.set(sessionId, revision);
  for (let retry = 0; retry < 2; retry += 1) {
    const requestId = createTerminalRequestId();
    let result: TerminalInputRouteResult;
    try {
      result = await awaitRouteClaim(destination.claimInputRoute(create(TerminalInputRouteClaimSchema, {
        requestId,
        sessionId,
        revision,
        domainGeneration: destination.token.domainGeneration,
        workerEpoch: destination.workerEpoch,
      })));
    } catch {
      if (state.claimId !== claimId) {
        return { accepted: false, unsupported: false, reason: "terminal input route claim was superseded" };
      }
      state.phase = "blocked";
      return { accepted: false, unsupported: false, reason: "terminal input route claim was not confirmed" };
    }
    if (state.claimId !== claimId || state.activeDestination !== destination) {
      return { accepted: false, unsupported: false, reason: "terminal input route claim was superseded" };
    }
    if (
      result.requestId !== requestId
      || result.sessionId !== sessionId
      || result.revision !== revision
      || result.workerEpoch !== destination.workerEpoch
    ) {
      state.phase = "blocked";
      return { accepted: false, unsupported: false, reason: "terminal input route claim response was invalid" };
    }
    if (result.accepted && result.inputRouteEpoch) {
      state.routeEpoch = result.inputRouteEpoch;
      state.routeTokenKey = destinationTokenKey;
      state.phase = "holding";
      return { accepted: true, inputRouteEpoch: result.inputRouteEpoch, revision };
    }
    if (result.reason !== "stale_route_revision" || retry !== 0) {
      state.phase = "blocked";
      return { accepted: false, unsupported: false, reason: result.reason || "terminal input route claim was rejected" };
    }
    if (result.latestRevision < revision || result.latestRevision >= MAX_TERMINAL_INPUT_ROUTE_REVISION) {
      state.phase = "blocked";
      return { accepted: false, unsupported: false, reason: "terminal input route revision is unavailable" };
    }
    revision = result.latestRevision + 1n;
    revisions.set(sessionId, revision);
  }
  state.phase = "blocked";
  return { accepted: false, unsupported: false, reason: "terminal input route claim was rejected" };
}

export function refreshTerminalInputRouteState(
  states: Iterable<TerminalInputRouteClaimState>,
  destination: TerminalInputDestination,
  destinationTokenKey: string,
  destinationConnectionKey: string,
  tokenKey: (token: TerminalGenerationToken) => string,
  connectionKey: (token: TerminalGenerationToken) => string,
): void {
  for (const state of states) {
    const previous = state.activeDestination;
    if (!previous || connectionKey(previous.token) !== destinationConnectionKey) continue;
    if (state.phase === "claiming") {
      state.claimId += 1;
      state.phase = "blocked";
      state.routeEpoch = "";
      state.routeTokenKey = null;
      state.activeDestination = { ...previous, token: destination.token };
      continue;
    }
    if (state.phase !== "sending" && state.phase !== "holding") continue;
    const previousTokenKey = tokenKey(previous.token);
    state.activeDestination = { ...previous, token: destination.token };
    if (state.routeTokenKey === previousTokenKey) state.routeTokenKey = destinationTokenKey;
  }

}

export function retireTerminalInputRouteState(
  state: TerminalInputRouteClaimState,
  matches: (token: TerminalGenerationToken) => boolean,
): void {
  if (!state.activeDestination || !matches(state.activeDestination.token)) return;
  state.phase = "blocked";
  state.claimId += 1;
  state.routeEpoch = "";
  state.routeTokenKey = null;
}
