// Bounded acknowledged input-route recovery after direct transport loss.
// A worker peer owner supplies its exact lifecycle predicate; this class owns
// one abort controller and one current input hold per session. It never retries
// bytes, only worker ownership claims for already-held fresh input.

import { terminalGenerationTokenEquals, type TerminalGenerationToken } from "../terminal-stream-types.ts";
import { rootStore } from "../root.ts";
import {
  claimTerminalInputRoute,
  holdTerminalInput,
  terminalInputConnectionKey,
  terminalInputRequiresRouteClaim,
  type TerminalInputDestination,
  type TerminalInputHold,
} from "./terminal-input-router.ts";
import {
  readySyncTerminalInputDestinationForSession,
  terminalInputDestinationForSession,
} from "./sync-outbound.ts";

const FALLBACK_CLAIM_ATTEMPTS = 120;
const FALLBACK_CLAIM_RETRY_MS = 250;

export interface TerminalPeerFallbackClaimOptions {
  readonly hold?: TerminalInputHold;
  readonly expectedOldConnection?: TerminalGenerationToken;
}

interface FallbackOperation {
  readonly controller: AbortController;
  readonly hold: TerminalInputHold;
  readonly authGeneration: number;
  readonly expectedOldConnection: TerminalGenerationToken | null;
}

/** Reclaims one current input hold only after an exact route-claim acknowledgement. */
export class TerminalPeerFallbackClaims {
  private readonly operations = new Map<string, FallbackOperation>();

  constructor(
    private readonly report: (reason: string | null) => void,
    private readonly isCurrent: () => boolean,
  ) {}

  claim(sessionId: string, options: TerminalPeerFallbackClaimOptions = {}): void {
    const existing = this.operations.get(sessionId);
    if (existing && this.sameIntent(existing, options)) return;
    this.retire(sessionId);
    const hold = options.hold ?? holdTerminalInput(sessionId);
    if (!hold.isCurrent()) return;
    const operation: FallbackOperation = {
      controller: new AbortController(),
      hold,
      authGeneration: rootStore.auth_generation,
      expectedOldConnection: options.expectedOldConnection ?? null,
    };
    this.operations.set(sessionId, operation);
    void this.recover(sessionId, operation).then((reason) => {
      if (this.owns(sessionId, operation)) this.report(reason);
    }).finally(() => {
      if (this.operations.get(sessionId) === operation) this.operations.delete(sessionId);
    });
  }

  retire(sessionId: string): void {
    const operation = this.operations.get(sessionId);
    if (!operation) return;
    this.operations.delete(sessionId);
    operation.controller.abort();
    if (operation.hold.isCurrent()) operation.hold.release();
  }

  dispose(): void {
    for (const operation of this.operations.values()) {
      operation.controller.abort();
      if (operation.hold.isCurrent()) operation.hold.release();
    }
    this.operations.clear();
  }

  private sameIntent(
    operation: FallbackOperation,
    options: TerminalPeerFallbackClaimOptions,
  ): boolean {
    if (!operation.hold.isCurrent() || operation.controller.signal.aborted) return false;
    const expected = options.expectedOldConnection ?? null;
    if (!expected || !operation.expectedOldConnection) return expected === operation.expectedOldConnection;
    return terminalInputConnectionKey(expected) === terminalInputConnectionKey(operation.expectedOldConnection);
  }

  private owns(sessionId: string, operation: FallbackOperation): boolean {
    return this.operations.get(sessionId) === operation
      && !operation.controller.signal.aborted
      && rootStore.auth_generation === operation.authGeneration
      && this.isCurrent();
  }

  private current(sessionId: string, operation: FallbackOperation): boolean {
    return this.owns(sessionId, operation) && operation.hold.isCurrent();
  }
  private destination(
    sessionId: string,
    expectedOldConnection: TerminalGenerationToken | null,
  ): TerminalInputDestination | null {
    if (!expectedOldConnection) return readySyncTerminalInputDestinationForSession(sessionId);
    const destination = terminalInputDestinationForSession(sessionId);
    if (
      !destination
      || terminalInputConnectionKey(destination.token) !== terminalInputConnectionKey(expectedOldConnection)
    ) return null;
    return destination;
  }

  private async recover(sessionId: string, operation: FallbackOperation): Promise<string | null> {
    let lastReason = "terminal Sync fallback input route was unavailable";
    for (let attempt = 0; attempt < FALLBACK_CLAIM_ATTEMPTS; attempt += 1) {
      if (!this.current(sessionId, operation)) return null;
      const destination = this.destination(sessionId, operation.expectedOldConnection);
      if (!destination) {
        if (operation.expectedOldConnection) {
          operation.hold.release();
          return lastReason;
        }
      } else if (!destination.inputRouteSupported) {
        if (!terminalInputRequiresRouteClaim(sessionId)) {
          operation.hold.release(destination);
          if (operation.hold.isCurrent()) operation.hold.release();
          return null;
        }
        lastReason = "terminal Sync fallback input-route capability is unavailable";
      } else {
        try {
          const claimed = await claimTerminalInputRoute(sessionId, destination);
          if (!this.current(sessionId, operation)) return null;
          const current = this.destination(sessionId, operation.expectedOldConnection);
          if (
            claimed.accepted
            && current
            && terminalGenerationTokenEquals(current.token, destination.token)
          ) {
            operation.hold.release(current);
            if (operation.hold.isCurrent()) operation.hold.release();
            return null;
          }
          lastReason = claimed.accepted
            ? "terminal Sync fallback input route changed"
            : claimed.reason;
        } catch {
          if (!this.current(sessionId, operation)) return null;
          lastReason = "terminal Sync fallback input route claim was not confirmed";
        }
      }
      if (!this.current(sessionId, operation)) return null;
      await new Promise<void>((resolve) => setTimeout(resolve, FALLBACK_CLAIM_RETRY_MS));
    }
    if (this.current(sessionId, operation)) operation.hold.release();
    return lastReason;
  }
}
