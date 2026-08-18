import {
  AgentStatusUpdate,
  type AgentRuntimeState,
  type AgentStatusUpdate as AgentStatusUpdateType,
} from "@roost/shared/wire";
import type { BuiltinAgentId } from "./process-scan.ts";

export const INTEGRATION_LEASE_MS = 30_000;

export interface IntegrationStatusReport {
  sessionId: string;
  agentId: BuiltinAgentId;
  state: AgentRuntimeState;
  message?: string;
  seq: number;
  active: boolean;
}

interface IntegrationEntry {
  agentId: BuiltinAgentId;
  state: AgentRuntimeState;
  message?: string;
  seq: number;
  leaseUntil: number;
}

interface ScreenEntry {
  agentId: BuiltinAgentId;
  state: AgentRuntimeState;
}

interface EffectiveEntry {
  agentId: BuiltinAgentId;
  state: AgentRuntimeState;
  message?: string;
  revision: number;
  completedRevision: number;
  updatedAt: number;
}

interface SessionEntry {
  integration?: IntegrationEntry;
  lastIntegrationSeq: number;
  screen?: ScreenEntry;
  effective?: EffectiveEntry;
}

export interface AgentStatusRegistryOptions {
  publish: (status: AgentStatusUpdateType) => void;
  now?: () => number;
  leaseMs?: number;
  startLeaseTimer?: boolean;
}

export class AgentStatusRegistry {
  private readonly entries = new Map<string, SessionEntry>();
  private readonly publish: (status: AgentStatusUpdateType) => void;
  private readonly now: () => number;
  private readonly leaseMs: number;
  private revision = Math.floor(Date.now() * 1_000);
  private leaseTimer: ReturnType<typeof setInterval> | null = null;

  constructor(options: AgentStatusRegistryOptions) {
    this.publish = options.publish;
    this.now = options.now ?? Date.now;
    this.leaseMs = options.leaseMs ?? INTEGRATION_LEASE_MS;
    if (options.startLeaseTimer !== false) {
      this.leaseTimer = setInterval(() => this.expireLeases(), 1_000);
      this.leaseTimer.unref?.();
    }
  }

  private entry(sessionId: string): SessionEntry {
    let entry = this.entries.get(sessionId);
    if (!entry) {
      entry = { lastIntegrationSeq: -1 };
      this.entries.set(sessionId, entry);
    }
    return entry;
  }

  private nextRevision(now: number): number {
    this.revision = Math.max(this.revision + 1, Math.floor(now * 1_000));
    return this.revision;
  }

  private recompute(sessionId: string, now = this.now()): void {
    const entry = this.entries.get(sessionId);
    if (!entry) return;
    const integration = entry.integration && entry.integration.leaseUntil > now
      ? entry.integration
      : undefined;
    if (entry.integration && !integration) entry.integration = undefined;
    const candidate = integration ?? entry.screen;
    const previous = entry.effective;
    if (!candidate) {
      if (!previous) return;
      const revision = this.nextRevision(now);
      entry.effective = undefined;
      this.publish(AgentStatusUpdate.parse({
        session_id: sessionId,
        agent_id: previous.agentId,
        state: previous.state,
        message: previous.message,
        revision,
        completed_revision: previous.completedRevision,
        updated_at: now,
        active: false,
      }));
      return;
    }

    const message = integration?.message;
    if (previous
      && previous.agentId === candidate.agentId
      && previous.state === candidate.state
      && previous.message === message) return;

    const revision = this.nextRevision(now);
    const completedRevision = previous
      && (previous.state === "working" || previous.state === "blocked")
      && candidate.state === "idle"
      ? revision
      : (previous?.completedRevision ?? 0);
    const effective: EffectiveEntry = {
      agentId: candidate.agentId,
      state: candidate.state,
      message,
      revision,
      completedRevision,
      updatedAt: now,
    };
    entry.effective = effective;
    this.publish(AgentStatusUpdate.parse({
      session_id: sessionId,
      agent_id: effective.agentId,
      state: effective.state,
      message: effective.message,
      revision: effective.revision,
      completed_revision: effective.completedRevision,
      updated_at: effective.updatedAt,
      active: true,
    }));
  }

  reportIntegration(report: IntegrationStatusReport): boolean {
    const entry = this.entry(report.sessionId);
    if (report.seq <= entry.lastIntegrationSeq) return false;
    entry.lastIntegrationSeq = report.seq;
    const now = this.now();
    if (report.active) {
      entry.integration = {
        agentId: report.agentId,
        state: report.state,
        message: report.message,
        seq: report.seq,
        leaseUntil: now + this.leaseMs,
      };
    } else {
      entry.integration = undefined;
    }
    this.recompute(report.sessionId, now);
    return true;
  }

  reportScreen(sessionId: string, report: ScreenEntry): void {
    const entry = this.entry(sessionId);
    entry.screen = report;
    this.recompute(sessionId);
  }

  clearScreen(sessionId: string): void {
    const entry = this.entries.get(sessionId);
    if (!entry || !entry.screen) return;
    entry.screen = undefined;
    this.recompute(sessionId);
  }

  expireLeases(now = this.now()): void {
    for (const [sessionId, entry] of this.entries) {
      if (entry.integration && entry.integration.leaseUntil <= now) this.recompute(sessionId, now);
    }
  }

  closeSession(sessionId: string): void {
    const entry = this.entries.get(sessionId);
    if (!entry) return;
    entry.integration = undefined;
    entry.screen = undefined;
    this.recompute(sessionId);
    this.entries.delete(sessionId);
  }

  retainSessions(sessionIds: ReadonlySet<string>): void {
    for (const sessionId of this.entries.keys()) {
      if (!sessionIds.has(sessionId)) this.closeSession(sessionId);
    }
  }

  resend(): void {
    for (const [sessionId, entry] of this.entries) {
      const effective = entry.effective;
      if (!effective) continue;
      this.publish(AgentStatusUpdate.parse({
        session_id: sessionId,
        agent_id: effective.agentId,
        state: effective.state,
        message: effective.message,
        revision: effective.revision,
        completed_revision: effective.completedRevision,
        updated_at: effective.updatedAt,
        active: true,
      }));
    }
  }

  snapshot(): AgentStatusUpdateType[] {
    const statuses: AgentStatusUpdateType[] = [];
    const originalPublish = this.publish;
    void originalPublish;
    for (const [sessionId, entry] of this.entries) {
      const effective = entry.effective;
      if (!effective) continue;
      statuses.push(AgentStatusUpdate.parse({
        session_id: sessionId,
        agent_id: effective.agentId,
        state: effective.state,
        message: effective.message,
        revision: effective.revision,
        completed_revision: effective.completedRevision,
        updated_at: effective.updatedAt,
        active: true,
      }));
    }
    return statuses;
  }

  dispose(): void {
    if (this.leaseTimer) clearInterval(this.leaseTimer);
    this.leaseTimer = null;
    this.entries.clear();
  }
}
