// Adapted from Herdr src/pane/agent_detection.rs at commit
// eacea2daf0b72973173b728936b27478374f2cd2 (Apache-2.0).

import type { AgentRuntimeState } from "@roost/shared/wire";
import type { BuiltinAgentId } from "./process-scan.ts";
import type { ManifestDetection } from "./manifest-engine.ts";

const PENDING_IDLE_CONFIRMATIONS = 3;
const PENDING_IDLE_CAP_MS = 700;

export interface StableScreenReport {
  agentId: BuiltinAgentId;
  state: AgentRuntimeState;
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
    agentId: BuiltinAgentId,
    detection: ManifestDetection,
    now = Date.now(),
  ): StableScreenReport | null {
    const previous = this.entries.get(sessionId);
    const agentChanged = previous !== undefined && previous.agentId !== agentId;
    if (detection.skipStateUpdate || detection.state === "unknown") {
      if (agentChanged) this.entries.delete(sessionId);
      return null;
    }

    const next: StableEntry = {
      agentId,
      state: detection.state,
      visibleIdle: detection.visibleIdle,
      visibleBlocker: detection.visibleBlocker,
      visibleWorking: detection.visibleWorking,
      pendingIdleStartedAt: null,
      pendingIdleConfirmations: 0,
    };
    if (!previous || agentChanged) {
      this.entries.set(sessionId, next);
      return { agentId, state: next.state };
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
    return changed ? { agentId, state: next.state } : null;
  }

  current(sessionId: string): StableScreenReport | null {
    const entry = this.entries.get(sessionId);
    return entry ? { agentId: entry.agentId, state: entry.state } : null;
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
