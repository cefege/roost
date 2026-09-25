// In-process-only terminal peer faults for the disposable smoke worker entrypoint.
// The ordinary worker neither creates nor receives this state, so no environment
// variable, public command, or persistent control endpoint can arm a fault.
// Each one-shot mutation is consumed at its real owner boundary and cleanup releases holds.

import { randomUUID } from "node:crypto";
import { acquireKeeperAdmission, type KeeperAdmissionTicket } from "../../session/session-control-lanes.ts";
import type { SessionManager } from "../../session/session-manager.ts";
import { monoNowMs } from "../../util/mono.ts";
import type { TerminalPeerOwner } from "./terminal-peer-owner.ts";
import type { TerminalPeerMalformedPacketKind } from "./terminal-peer-packet-test-fault.ts";

export type TerminalPeerOfferFault =
  | "invalid_sdp"
  | "missing_grant"
  | "expired_grant"
  | "identity_mismatch";

interface AdmissionHold {
  readonly ticket: KeeperAdmissionTicket;
  readonly sessionId: string;
}

interface PendingHistoryHold {
  readonly sessionId: string;
  readonly holdId: string;
  deliver: boolean | null;
}

interface CapturedHistoryHold {
  readonly holdId: string;
  readonly resolveDelivery: (deliver: boolean) => void;
}

/** Mutable only inside a smoke worker process; callers get commands through a disposable socket. */
export class TerminalPeerTestFaultState {
  #nextOfferFault: TerminalPeerOfferFault | null = null;
  #blackholePackets = false;
  #dropNextInputResult = false;
  #dropNextDirectRetire = false;
  #clockOffsetMs = 0;
  #sweepExpiredGrants: (() => void) | null = null;
  #shrinkGrantScope: ((sessionId: string) => number) | null = null;
  readonly #admissionHolds = new Map<string, AdmissionHold>();
  #pendingHistoryHold: PendingHistoryHold | null = null;
  #capturedHistoryHold: CapturedHistoryHold | null = null;
  #peerOwner: TerminalPeerOwner | null = null;

  now(): number {
    return monoNowMs() + this.#clockOffsetMs;
  }

  attachGrantExpirySweep(sweep: () => void): void {
    this.#sweepExpiredGrants = sweep;
  }


  attachPeerOwner(peerOwner: TerminalPeerOwner): void {
    this.#peerOwner = peerOwner;
  }
  attachGrantScopeShrink(shrink: (sessionId: string) => number): void {
    this.#shrinkGrantScope = shrink;
  }

  armOfferFault(fault: TerminalPeerOfferFault): void {
    this.#nextOfferFault = fault;
  }

  consumeOfferFault(): TerminalPeerOfferFault | null {
    const fault = this.#nextOfferFault;
    this.#nextOfferFault = null;
    return fault;
  }

  setPacketBlackhole(enabled: boolean): void {
    this.#blackholePackets = enabled;
  }

  peerPacketsBlackholed(): boolean {
    return this.#blackholePackets;
  }

  dropNextPeerInputResult(): void {
    this.#dropNextInputResult = true;
  }

  consumePeerInputResultDrop(): boolean {
    if (!this.#dropNextInputResult) return false;
    this.#dropNextInputResult = false;
    return true;
  }

  advanceGrantClock(milliseconds: number): void {
    if (!Number.isSafeInteger(milliseconds) || milliseconds <= 0) {
      throw new Error("grant clock advance must be a positive safe integer");
    }
    this.#clockOffsetMs += milliseconds;
    if (!Number.isSafeInteger(this.#clockOffsetMs)) {
      throw new Error("grant clock advance exceeds the supported range");
    }
    this.#sweepExpiredGrants?.();
  }

  shrinkGrantForSession(sessionId: string): number {
    if (!this.#shrinkGrantScope) throw new Error("grant scope shrink is unavailable");
    return this.#shrinkGrantScope(sessionId);
  }

  injectMalformedPacket(kind: TerminalPeerMalformedPacketKind): void {
    if (!this.#peerOwner?.injectMalformedPacketForTest(kind)) {
      throw new Error("no authenticated terminal peer is available for malformed packet injection");
    }
  }

  setHistoryDeliveryPaused(paused: boolean): void {
    this.#peerOwner?.setHistoryDeliveryPausedForTest(paused);
  }


  holdNextHistoryResponse(sessionId: string): string {
    if (this.#pendingHistoryHold || this.#capturedHistoryHold) {
      throw new Error("terminal peer history hold already owns a response");
    }
    const holdId = randomUUID();
    this.#pendingHistoryHold = { sessionId, holdId, deliver: null };
    return holdId;
  }

  async holdPeerHistoryResponse(sessionId: string): Promise<boolean> {
    const pending = this.#pendingHistoryHold;
    if (!pending || pending.sessionId !== sessionId) return true;
    this.#pendingHistoryHold = null;
    if (pending.deliver !== null) return pending.deliver;
    const delivery = new Promise<boolean>((resolveDelivery) => {
      this.#capturedHistoryHold = { holdId: pending.holdId, resolveDelivery };
    });
    return await delivery;
  }

  releaseHistoryResponse(holdId: string, deliver: boolean): void {
    const pending = this.#pendingHistoryHold;
    if (pending?.holdId === holdId) {
      pending.deliver = deliver;
      return;
    }
    const captured = this.#capturedHistoryHold;
    if (!captured || captured.holdId !== holdId) return;
    this.#capturedHistoryHold = null;
    captured.resolveDelivery(deliver);
  }
  async holdKeeperAdmission(sessions: SessionManager, sessionId: string): Promise<string> {
    const record = sessions.getBySessionId(sessionId);
    if (!record) throw new Error("terminal session is unavailable for admission hold");
    const ticketResult = acquireKeeperAdmission(sessions, record.channelId, "query_reply");
    if (!ticketResult.admitted) throw new Error(ticketResult.reason);
    await ticketResult.ticket.granted;
    const holdId = randomUUID();
    this.#admissionHolds.set(holdId, { ticket: ticketResult.ticket, sessionId });
    return holdId;
  }

  releaseKeeperAdmission(holdId: string): void {
    const hold = this.#admissionHolds.get(holdId);
    if (!hold) return;
    this.#admissionHolds.delete(holdId);
    hold.ticket.release();
  }

  armDirectRetireDrop(): void {
    this.#dropNextDirectRetire = true;
  }

  consumeDirectRetireDrop(): boolean {
    if (!this.#dropNextDirectRetire) return false;
    this.#dropNextDirectRetire = false;
    return true;
  }

  dispose(): void {
    for (const holdId of this.#admissionHolds.keys()) this.releaseKeeperAdmission(holdId);
    this.#sweepExpiredGrants = null;
    this.#shrinkGrantScope = null;
    this.#pendingHistoryHold = null;
    this.releaseHistoryResponse(this.#capturedHistoryHold?.holdId ?? "", false);
    this.#nextOfferFault = null;
    this.#peerOwner = null;
    this.#blackholePackets = false;
    this.#dropNextInputResult = false;
    this.#dropNextDirectRetire = false;
  }
}
