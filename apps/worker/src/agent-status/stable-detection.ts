// Stabilizes screen-derived state for one observed agent process. The process
// identity is part of the key so PID replacement cannot inherit pending idle
// confirmation or disappear behind an unchanged agent kind and state.

import type { AgentRuntimeState } from "@roost/shared/wire";
import type { AgentProcessIdentity, BuiltinAgentId } from "./process-scan.ts";
import type { ManifestDetection } from "./manifest-engine.ts";

const PENDING_IDLE_CONFIRMATIONS = 3;
const PENDING_IDLE_CAP_MS = 700;

export interface StableScreenReport {
  agentId: BuiltinAgentId;
  state: AgentRuntimeState;
  processId: number;
}

interface StableEntry extends StableScreenReport {
  visibleIdle: boolean;
  visibleBlocker: boolean;
  visibleWorking: boolean;
  pendingIdleStartedAt: number | null;
  pendingIdleConfirmations: number;
}

export class StableScreenDetector {
  private entries = new Map<string, StableEntry>();

  observe(
    sessionId: string,
    identity: AgentProcessIdentity,
    detection: ManifestDetection,
    now = Date.now(),
  ): StableScreenReport | null {
    const previous = this.entries.get(sessionId);
    const identityChanged = previous !== undefined
      && (previous.agentId !== identity.agentId || previous.processId !== identity.pid);
    if (detection.skipStateUpdate || detection.state === "unknown") {
      if (identityChanged) this.entries.delete(sessionId);
      return null;
    }

    const next: StableEntry = {
      agentId: identity.agentId,
      processId: identity.pid,
      state: detection.state,
      visibleIdle: detection.visibleIdle,
      visibleBlocker: detection.visibleBlocker,
      visibleWorking: detection.visibleWorking,
      pendingIdleStartedAt: null,
      pendingIdleConfirmations: 0,
    };
    if (!previous || identityChanged) {
      this.entries.set(sessionId, next);
      return { agentId: identity.agentId, processId: identity.pid, state: next.state };
    }

    const plainWorkingToIdle = previous.state === "working"
      && next.state === "idle"
      && !next.visibleIdle
      && !next.visibleBlocker;
    if (plainWorkingToIdle) {
      if (previous.pendingIdleStartedAt === null) {
        previous.pendingIdleStartedAt = now;
        previous.pendingIdleConfirmations = 0;
        return null;
      }
      if (now - previous.pendingIdleStartedAt < PENDING_IDLE_CAP_MS) {
        previous.pendingIdleConfirmations++;
        if (previous.pendingIdleConfirmations < PENDING_IDLE_CONFIRMATIONS) return null;
      }
    }

    const changed = previous.state !== next.state
      || previous.visibleIdle !== next.visibleIdle
      || previous.visibleBlocker !== next.visibleBlocker
      || previous.visibleWorking !== next.visibleWorking;
    this.entries.set(sessionId, next);
    return changed
      ? { agentId: identity.agentId, processId: identity.pid, state: next.state }
      : null;
  }

  current(sessionId: string): StableScreenReport | null {
    const entry = this.entries.get(sessionId);
    return entry
      ? { agentId: entry.agentId, processId: entry.processId, state: entry.state }
      : null;
  }

  release(sessionId: string): void {
    this.entries.delete(sessionId);
  }

  retain(sessionIds: ReadonlySet<string>): void {
    for (const sessionId of this.entries.keys()) {
      if (!sessionIds.has(sessionId)) this.entries.delete(sessionId);
    }
  }
}
