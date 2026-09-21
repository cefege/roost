// Per-worker staged direct-route promotions. TerminalPeerOwner supplies demand
// and lifecycle fences while this owner coordinates old-route drains, claims,
// canonical commits, cancellation, and bounded fallback ownership recovery.
// It owns no RTC connection, grant, registry registration, or heartbeat lifecycle.

import { currentTerminalGenerationToken } from "../store/terminal-stream-publication.ts";
import {
  createTerminalSessionPromotion,
  type TerminalSessionPromotion,
} from "../store/terminal-stream-promotion.ts";
import {
  type TerminalDirectConnection,
  type TerminalDirectRegistry,
} from "../store/terminal-stream-transport.ts";
import {
  terminalGenerationTokenEquals,
  type TerminalGenerationToken,
} from "../store/terminal-stream-types.ts";
import { rootStore } from "../store/root.ts";
import { createTerminalDirectRequestId } from "./terminal-direct-browser.ts";
import type { TerminalPeerFallbackClaims } from "./terminal-peer-fallback.ts";
import {
  claimTerminalInputRoute,
  drainTerminalInput,
  holdTerminalInput,
  terminalInputConnectionKey,
  terminalInputRequiresRouteClaim,
  type TerminalInputHold,
} from "./terminal-input-router.ts";
import { terminalInputDestinationForDirectConnection, terminalInputDestinationForSession } from "./sync-outbound.ts";
import { terminalPeerDeadline } from "./terminal-peer-runtime.ts";

const INPUT_HANDOFF_DRAIN_MS = 10_000;

export interface TerminalPeerPromotionsHooks {
  hasDemand(sessionId: string): boolean;
  isCurrent(): boolean;
  committed(connection: TerminalDirectConnection): void;
  failed(reason: string): void;
}

interface PromotionRun {
  readonly candidate: TerminalSessionPromotion;
  readonly connection: TerminalDirectConnection;
  readonly candidateToken: TerminalGenerationToken;
  readonly oldToken: TerminalGenerationToken | null;
  readonly authGeneration: number;
  hold: TerminalInputHold | null;
  claimMayHaveReached: boolean;
  recoveryStarted: boolean;
  promoting: boolean;
  committed: boolean;
}

/** Coordinates one staged candidate per demanded session for one worker. */
export class TerminalPeerPromotions {
  private readonly runs = new Map<string, PromotionRun>();
  private disposed = false;

  constructor(
    private readonly workerFp: string,
    private readonly registry: TerminalDirectRegistry,
    private readonly recovery: TerminalPeerFallbackClaims,
    private readonly hooks: TerminalPeerPromotionsHooks,
  ) {}

  stage(sessionId: string, connection: TerminalDirectConnection): void {
    if (this.disposed || !this.hooks.isCurrent() || !this.hooks.hasDemand(sessionId)) return;
    const candidateToken = connection.token();
    if (
      !candidateToken
      || candidateToken.workerFp !== this.workerFp
      || !connection.allowsSession(sessionId)
      || this.registry.activeForSession(sessionId) === connection
    ) return;
    const existing = this.runs.get(sessionId);
    if (
      existing?.connection === connection
      && terminalGenerationTokenEquals(existing.candidateToken, candidateToken)
      && (existing.promoting || !existing.candidate.isReady())
    ) return;
    this.cancelPromotion(sessionId, "candidate replaced");
    const attemptId = createTerminalDirectRequestId();
    const candidate = createTerminalSessionPromotion({
      sessionId,
      attemptId,
      connection,
      token: candidateToken,
      onCancelled: (reason) => this.candidateCancelled(sessionId, attemptId, reason),
    });
    if (!candidate) return;
    const run: PromotionRun = {
      candidate,
      connection,
      candidateToken,
      oldToken: currentTerminalGenerationToken(sessionId),
      authGeneration: rootStore.auth_generation,
      hold: null,
      claimMayHaveReached: false,
      recoveryStarted: false,
      promoting: false,
      committed: false,
    };

    this.runs.set(sessionId, run);
    void candidate.awaitReady().then((ready) => {
      if (ready) void this.promote(sessionId, run);
    }).catch(() => this.cancelPromotion(sessionId, "candidate readiness failed", run));
  }

  cancelSession(sessionId: string, reason: string): void {
    this.cancelPromotion(sessionId, reason);
  }

  retireConnection(connection: TerminalDirectConnection, reason: string): void {
    const connectionToken = connection.token();
    for (const [sessionId, run] of [...this.runs]) {
      if (run.connection === connection) {
        this.cancelPromotion(sessionId, reason, run);
        continue;
      }
      if (
        connectionToken
        && run.oldToken
        && terminalInputConnectionKey(connectionToken) === terminalInputConnectionKey(run.oldToken)
      ) this.cancelForLostOldRoute(sessionId, reason, run);
    }
  }

  dispose(reason: string): void {
    if (this.disposed) return;
    this.disposed = true;
    for (const [sessionId, run] of [...this.runs]) {
      this.runs.delete(sessionId);
      if (run.hold?.isCurrent()) run.hold.release();
      run.candidate.cancel(reason);
    }
  }

  private async promote(sessionId: string, run: PromotionRun): Promise<void> {
    if (run.promoting || !this.current(sessionId, run)) return;
    if (!this.matchesOldToken(sessionId, run)) return this.restart(sessionId, run);
    run.promoting = true;
    run.hold = holdTerminalInput(sessionId);
    try {
      if (!this.current(sessionId, run, true)) return;
      if (run.oldToken) await terminalPeerDeadline(drainTerminalInput(sessionId, run.oldToken), INPUT_HANDOFF_DRAIN_MS);
      if (!this.current(sessionId, run, true)) return;
      if (!this.matchesOldToken(sessionId, run)) return this.restart(sessionId, run);
      const destination = terminalInputDestinationForDirectConnection(run.connection);
      if (!destination || !terminalGenerationTokenEquals(destination.token, run.candidateToken)) {
        throw new Error("candidate direct connection lost its token");
      }
      let claimEpoch = "";
      if (!destination.inputRouteSupported) {
        if (terminalInputRequiresRouteClaim(sessionId)) {
          throw new Error("terminal peer input-route capability is unavailable");
        }
      } else {
        if (!this.current(sessionId, run, true) || !this.matchesOldToken(sessionId, run)) {
          return this.restart(sessionId, run);
        }
        run.claimMayHaveReached = true;
        const claim = await claimTerminalInputRoute(sessionId, destination);
        if (!this.current(sessionId, run, true)) return;
        if (!this.matchesOldToken(sessionId, run)) return this.restart(sessionId, run);
        if (!claim.accepted) throw new Error(claim.reason);
        claimEpoch = claim.inputRouteEpoch;
      }
      if (!this.current(sessionId, run, true) || !this.matchesOldToken(sessionId, run)) {
        return this.restart(sessionId, run);
      }
      const prepared = run.candidate.prepare(claimEpoch, run.oldToken);
      if (!prepared || !this.registry.commitSessionPromotion(sessionId, run.candidate.attemptId, prepared)) {
        throw new Error("candidate promotion was superseded");
      }
      run.committed = true;
      this.runs.delete(sessionId);
      run.hold.release(destination);
      if (run.hold.isCurrent()) run.hold.release();
      this.hooks.committed(run.connection);
    } catch (error) {
      this.cancelPromotion(
        sessionId,
        error instanceof Error ? error.message : String(error),
        run,
      );
    } finally {
      run.promoting = false;
      if (!run.committed && this.runs.get(sessionId) === run) {
        this.cancelPromotion(sessionId, "candidate promotion did not commit", run);
      }
    }
  }

  private current(sessionId: string, run: PromotionRun, requireHold = false): boolean {
    return !this.disposed
      && this.runs.get(sessionId) === run
      && !run.committed
      && rootStore.auth_generation === run.authGeneration
      && this.hooks.isCurrent()
      && this.hooks.hasDemand(sessionId)
      && run.candidate.isReady()
      && terminalGenerationTokenEquals(run.connection.token(), run.candidateToken)
      && (!requireHold || run.hold?.isCurrent() === true);
  }

  private matchesOldToken(sessionId: string, run: PromotionRun): boolean {
    return terminalGenerationTokenEquals(currentTerminalGenerationToken(sessionId), run.oldToken);
  }

  private restart(sessionId: string, run: PromotionRun): void {
    if (this.runs.get(sessionId) !== run) return;
    const currentOldToken = currentTerminalGenerationToken(sessionId);
    const canRestage = !run.claimMayHaveReached
      && currentOldToken !== null
      && this.hooks.isCurrent()
      && this.hooks.hasDemand(sessionId);
    this.cancelPromotion(sessionId, "terminal input route changed", run);
    if (canRestage) this.stage(sessionId, run.connection);
  }

  private candidateCancelled(sessionId: string, attemptId: string, reason: string): void {
    const run = this.runs.get(sessionId);
    if (!run || run.candidate.attemptId !== attemptId || run.committed) return;
    this.runs.delete(sessionId);
    this.hooks.failed(reason);
    this.recoverOrRelease(sessionId, run);
  }

  private cancelPromotion(sessionId: string, reason: string, expected?: PromotionRun): void {
    const run = this.runs.get(sessionId);
    if (!run || (expected && run !== expected)) return;
    this.runs.delete(sessionId);
    this.recoverOrRelease(sessionId, run);
    run.candidate.cancel(reason);
    this.hooks.failed(reason);
  }

  private cancelForLostOldRoute(sessionId: string, reason: string, run: PromotionRun): void {
    if (this.runs.get(sessionId) !== run) return;
    this.runs.delete(sessionId);
    this.recover(sessionId, run, false);
    run.candidate.cancel(reason);
    this.hooks.failed(reason);
  }

  private recoverOrRelease(sessionId: string, run: PromotionRun): void {
    if (run.claimMayHaveReached) {
      this.recover(sessionId, run, true);
      return;
    }
    this.releaseCurrentOldRoute(sessionId, run);
  }

  private recover(sessionId: string, run: PromotionRun, expectedOldConnection: boolean): void {
    if (run.recoveryStarted) return;
    run.recoveryStarted = true;
    this.recovery.claim(sessionId, {
      ...(run.hold ? { hold: run.hold } : {}),
      ...(expectedOldConnection && run.oldToken ? { expectedOldConnection: run.oldToken } : {}),
    });
  }

  private releaseCurrentOldRoute(sessionId: string, run: PromotionRun): void {
    const hold = run.hold;
    if (!hold?.isCurrent()) return;
    const currentToken = currentTerminalGenerationToken(sessionId);
    const destination = terminalInputDestinationForSession(sessionId);
    if (
      !currentToken
      || !destination
      || !terminalGenerationTokenEquals(currentToken, destination.token)
      || (
        run.oldToken !== null
        && terminalInputConnectionKey(currentToken) !== terminalInputConnectionKey(run.oldToken)
      )
    ) {
      hold.release();
      return;
    }
    hold.release(destination);
    if (hold.isCurrent()) hold.release();
  }
}
