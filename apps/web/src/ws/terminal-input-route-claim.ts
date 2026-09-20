// Input-route claim sequencing for the document terminal input router. The
// router passes its session state and token key function so this module never
// owns a second route table or transport callback. It creates UUID request IDs,
// validates every reply, and permits exactly one server-directed stale retry.

import { create } from "@bufbuild/protobuf";
import {
  TerminalInputRouteClaimSchema,
  type TerminalInputRouteResult,
} from "@roost/shared/proto/sync_pb";
import type { TerminalInputDestination } from "./terminal-input-router.ts";

export const MAX_TERMINAL_INPUT_ROUTE_REVISION = (1n << 63n) - 1n;

export type TerminalInputPhase = "sending" | "holding" | "claiming" | "blocked" | "ambiguous" | "closed";

export type TerminalInputRouteClaimOutcome =
  | { accepted: true; inputRouteEpoch: string; revision: bigint }
  | { accepted: false; unsupported: true; reason: string }
  | { accepted: false; unsupported: false; reason: string };

export interface TerminalInputRouteClaimState {
  phase: TerminalInputPhase;
  claimId: number;
  routeEpoch: string;
  routeTokenKey: string | null;
  activeDestination: TerminalInputDestination | null;
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
  tokenKey: (token: TerminalInputDestination["token"]) => string,
  connectionKey: (token: TerminalInputDestination["token"]) => string,
): void {
  for (const state of states) {
    const previous = state.activeDestination;
    if (
      !previous
      || (state.phase !== "sending" && state.phase !== "holding")
      || connectionKey(previous.token) !== destinationConnectionKey
    ) continue;
    const previousTokenKey = tokenKey(previous.token);
    state.activeDestination = { ...previous, token: destination.token };
    if (state.routeTokenKey === previousTokenKey) state.routeTokenKey = destinationTokenKey;
  }
}
