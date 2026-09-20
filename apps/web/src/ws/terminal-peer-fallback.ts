// Direct-route loss reclaims worker input ownership for the Sync fallback.
// TerminalPeerOwner owns one instance per worker; this class fences concurrent
// session retries and prevents a retired loop from claiming after replacement.

import { terminalGenerationTokenEquals } from "../store/terminal-stream-types.ts";
import {
  claimTerminalInputRoute,
  holdTerminalInput,
  terminalInputPhase,
} from "./terminal-input-router.ts";
import { terminalInputDestinationForSession } from "./sync-outbound.ts";

const FALLBACK_CLAIM_ATTEMPTS = 120;
const FALLBACK_CLAIM_RETRY_MS = 250;

export class TerminalPeerFallbackClaims {
  private readonly controllers = new Map<string, AbortController>();

  constructor(private readonly report: (reason: string | null) => void) {}

  claim(sessionId: string): void {
    if (terminalInputPhase(sessionId) === "ambiguous") {
      this.report("terminal input outcome is ambiguous");
      return;
    }
    this.retire(sessionId);
    const controller = new AbortController();
    this.controllers.set(sessionId, controller);
    void claimTerminalInputFallback(sessionId, controller.signal, this.report).then((reason) => {
      if (this.controllers.get(sessionId) === controller) this.report(reason);
    }).finally(() => {
      if (this.controllers.get(sessionId) === controller) this.controllers.delete(sessionId);
    });
  }

  retire(sessionId: string): void {
    this.controllers.get(sessionId)?.abort();
    this.controllers.delete(sessionId);
  }

  dispose(): void {
    for (const controller of this.controllers.values()) controller.abort();
    this.controllers.clear();
  }
}

async function claimTerminalInputFallback(
  sessionId: string,
  signal: AbortSignal,
  reportFailure: (reason: string) => void,
): Promise<string | null> {
  const release = holdTerminalInput(sessionId);
  let lastReason = "terminal Sync fallback input route was unavailable";
  for (let attempt = 0; attempt < FALLBACK_CLAIM_ATTEMPTS; attempt += 1) {
    if (signal.aborted) {
      release();
      return null;
    }
    const destination = terminalInputDestinationForSession(sessionId);
    if (destination && !destination.inputRouteSupported) {
      if (signal.aborted) {
        release();
        return null;
      }
      release(destination);
      return null;
    }
    if (destination) {
      try {
        const claimed = await claimTerminalInputRoute(sessionId, destination);
        if (signal.aborted) {
          release();
          return null;
        }
        const current = terminalInputDestinationForSession(sessionId);
        if (
          claimed.accepted
          && current
          && terminalGenerationTokenEquals(current.token, destination.token)
        ) {
          release(current);
          return null;
        }
        if (!claimed.accepted) {
          lastReason = claimed.reason;
          reportFailure(lastReason);
        }
      } catch (error) {
        lastReason = error instanceof Error ? error.message : String(error);
        reportFailure(lastReason);
      }
    }
    if (signal.aborted) {
      release();
      return null;
    }
    await new Promise<void>((resolve) => setTimeout(resolve, FALLBACK_CLAIM_RETRY_MS));
  }
  release();
  return lastReason;
}
