// Content-free direct-terminal probe correlation and liveness qualification.
// WebRTC and loopback adapters supply their control sends; this owner fences replies.
// It clears proof on timeout/resume and exposes only safe monotonic telemetry.

import type { TerminalTransportProbeResult } from "@roost/shared/proto/sync_pb";
import {
  TERMINAL_PEER_PROBE_DEADLINE_MS,
  TERMINAL_PEER_PROBE_QUALIFICATION_MS,
} from "@roost/shared/terminal-peer";
import { terminalDirectMonotonicNow } from "./terminal-direct-browser.ts";

interface PendingProbe {
  readonly resolve: () => void;
  readonly reject: (error: Error) => void;
  readonly timer: Timer;
  readonly startedAtMs: number;
}

export class TerminalPeerProbeState {
  private readonly pending = new Map<string, PendingProbe>();
  private lastProbeAtMs: number | null = null;
  private rttMs: number | null = null;
  private qualified = false;

  constructor(
    private readonly workerFp: string,
    private readonly workerEpoch: string,
  ) {}

  telemetry(): {
    lastProbeAtMs: number | null;
    rttMs: number | null;
    livenessQualified: boolean;
  } {
    return {
      lastProbeAtMs: this.lastProbeAtMs,
      rttMs: this.rttMs,
      livenessQualified: this.qualified
        && this.lastProbeAtMs !== null
        && terminalDirectMonotonicNow() - this.lastProbeAtMs <= TERMINAL_PEER_PROBE_QUALIFICATION_MS,
    };
  }

  requireFresh(): void {
    this.qualified = false;
    this.rejectAll("terminal peer probe episode replaced");
  }

  start(requestId: string, send: () => boolean): Promise<void> {
    if (!requestId || this.pending.has(requestId)) {
      return Promise.reject(new Error("terminal peer probe is unavailable"));
    }
    const startedAtMs = terminalDirectMonotonicNow();
    const promise = new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(requestId);
        this.qualified = false;
        reject(new Error("terminal peer probe timed out"));
      }, TERMINAL_PEER_PROBE_DEADLINE_MS);
      this.pending.set(requestId, { resolve, reject, timer, startedAtMs });
    });
    if (!send()) this.reject(requestId, "terminal peer did not accept the request");
    return promise;
  }

  resolve(result: TerminalTransportProbeResult): boolean {
    if (result.workerFp !== this.workerFp || result.workerEpoch !== this.workerEpoch) return false;
    const pending = this.pending.get(result.requestId);
    if (!pending) return false;
    clearTimeout(pending.timer);
    this.pending.delete(result.requestId);
    const receivedAtMs = terminalDirectMonotonicNow();
    this.lastProbeAtMs = receivedAtMs;
    this.rttMs = Math.max(0, receivedAtMs - pending.startedAtMs);
    this.qualified = true;
    pending.resolve();
    return true;
  }

  close(reason: string): void {
    this.qualified = false;
    this.rejectAll(reason);
  }

  private reject(requestId: string, reason: string): void {
    const pending = this.pending.get(requestId);
    if (!pending) return;
    clearTimeout(pending.timer);
    this.pending.delete(requestId);
    this.qualified = false;
    pending.reject(new Error(reason));
  }

  private rejectAll(reason: string): void {
    for (const [requestId, pending] of this.pending) {
      clearTimeout(pending.timer);
      this.pending.delete(requestId);
      pending.reject(new Error(reason));
    }
  }
}
