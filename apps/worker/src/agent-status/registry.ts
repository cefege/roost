// Aggregates ancestry-verified integration and screen observations into the
// identified AgentStatusUpdate frames sent to the coordinator. One registry
// owns one worker epoch; each uninterrupted agent-kind/PID incarnation owns
// one occupant token. Process IDs remain private to this module and its local
// producers.
import { randomUUID } from "node:crypto";
import {
  AgentOccupantId,
  AgentStatusUpdate,
  StatusEpoch,
  type AgentRuntimeState,
  type AgentStatusSource,
  type AgentStatusUpdate as AgentStatusUpdateType,
} from "@roost/shared/wire";
import { log } from "@roost/shared/log";
import type { BuiltinAgentId } from "./process-scan.ts";

export const INTEGRATION_LEASE_MS = 30_000;

export interface IntegrationStatusReport {
  sessionId: string;
  agentId: BuiltinAgentId;
  processId: number;
  state: AgentRuntimeState;
  message?: string;
  seq: number;
  active: boolean;
}

export interface ScreenStatusReport {
  agentId: BuiltinAgentId;
  processId: number;
  state: AgentRuntimeState;
}

interface ProcessCandidate {
  agentId: BuiltinAgentId;
  processId: number;
  processKey: string;
  state: AgentRuntimeState;
}

interface IntegrationCandidate extends ProcessCandidate {
  message?: string;
  seq: number;
  leaseUntil: number;
}

type ScreenCandidate = ProcessCandidate;

interface EffectiveEntry extends ProcessCandidate {
  message?: string;
  source: AgentStatusSource;
  occupantId: AgentOccupantId;
  revision: number;
  completedRevision: number;
  updatedAt: number;
}

interface SessionEntry {
  integration?: IntegrationCandidate;
  integrationSeqByProcess: Map<string, number>;
  screen?: ScreenCandidate;
  screenAbsenceObserved: boolean;
  effective?: EffectiveEntry;
  retiredProcessKeys: Set<string>;
}

export interface AgentStatusRegistryOptions {
  publish: (status: AgentStatusUpdateType) => void;
  now?: () => number;
  leaseMs?: number;
  startLeaseTimer?: boolean;
}

function processKey(agentId: BuiltinAgentId, processId: number): string {
  return `${agentId}:${processId}`;
}

export class AgentStatusRegistry {
  private readonly entries = new Map<string, SessionEntry>();
  private readonly publish: (status: AgentStatusUpdateType) => void;
  private readonly now: () => number;
  private readonly leaseMs: number;
  private readonly statusEpoch = StatusEpoch.parse(randomUUID());
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
      entry = {
        integrationSeqByProcess: new Map(),
        screenAbsenceObserved: true,
        retiredProcessKeys: new Set(),
      };
      this.entries.set(sessionId, entry);
    }
    return entry;
  }

  private nextRevision(now: number): number {
    this.revision = Math.max(this.revision + 1, Math.floor(now * 1_000));
    return this.revision;
  }

  private publishEffective(
    sessionId: string,
    effective: EffectiveEntry,
    active: boolean,
    revision = effective.revision,
    updatedAt = effective.updatedAt,
  ): void {
    this.publish(AgentStatusUpdate.parse({
      session_id: sessionId,
      agent_id: effective.agentId,
      state: effective.state,
      message: effective.message,
      revision,
      completed_revision: effective.completedRevision,
      updated_at: updatedAt,
      active,
      status_epoch: this.statusEpoch,
      occupant_id: effective.occupantId,
      source: effective.source,
    }));
  }

  private retireProcess(entry: SessionEntry, retiredKey: string): void {
    entry.retiredProcessKeys.add(retiredKey);
    if (entry.integration?.processKey === retiredKey) entry.integration = undefined;
    if (entry.screen?.processKey === retiredKey) entry.screen = undefined;
  }

  private reopenProcess(entry: SessionEntry, reopenedKey: string): void {
    entry.retiredProcessKeys.delete(reopenedKey);
    entry.integrationSeqByProcess.delete(reopenedKey);
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
      this.publishEffective(sessionId, previous, false, revision, now);
      log.info("agent-status", "occupant_inactive", {
        session_id: sessionId,
        agent_id: previous.agentId,
        status_epoch: this.statusEpoch,
        occupant_id: previous.occupantId,
        source: previous.source,
        state: previous.state,
        revision,
      });
      this.retireProcess(entry, previous.processKey);
      return;
    }

    const source: AgentStatusSource = integration ? "integration" : "screen";
    const message = integration?.message;
    const sameOccupant = previous?.processKey === candidate.processKey;
    if (sameOccupant
      && previous.state === candidate.state
      && previous.message === message
      && previous.source === source) return;

    if (previous && !sameOccupant) {
      const inactiveRevision = this.nextRevision(now);
      entry.effective = undefined;
      this.publishEffective(sessionId, previous, false, inactiveRevision, now);
      log.info("agent-status", "occupant_inactive", {
        session_id: sessionId,
        agent_id: previous.agentId,
        status_epoch: this.statusEpoch,
        occupant_id: previous.occupantId,
        source: previous.source,
        state: previous.state,
        revision: inactiveRevision,
      });
      this.retireProcess(entry, previous.processKey);
    }

    const revision = this.nextRevision(now);
    const completedRevision = sameOccupant
      && previous
      && (previous.state === "working" || previous.state === "blocked")
      && candidate.state === "idle"
      ? revision
      : (sameOccupant ? (previous?.completedRevision ?? 0) : 0);
    const effective: EffectiveEntry = {
      agentId: candidate.agentId,
      processId: candidate.processId,
      processKey: candidate.processKey,
      state: candidate.state,
      message,
      source,
      occupantId: sameOccupant && previous
        ? previous.occupantId
        : AgentOccupantId.parse(randomUUID()),
      revision,
      completedRevision,
      updatedAt: now,
    };
    entry.screenAbsenceObserved = false;
    entry.effective = effective;
    this.publishEffective(sessionId, effective, true);
    log.info("agent-status", sameOccupant ? "occupant_updated" : "occupant_active", {
      session_id: sessionId,
      agent_id: effective.agentId,
      status_epoch: this.statusEpoch,
      occupant_id: effective.occupantId,
      source: effective.source,
      state: effective.state,
      revision: effective.revision,
    });
  }

  reportIntegration(report: IntegrationStatusReport): boolean {
    const entry = this.entry(report.sessionId);
    const reporterKey = processKey(report.agentId, report.processId);
    if (entry.retiredProcessKeys.has(reporterKey)) return false;

    const previousSeq = entry.integrationSeqByProcess.get(reporterKey);
    if (previousSeq !== undefined && report.seq <= previousSeq) return false;
    entry.integrationSeqByProcess.set(reporterKey, report.seq);
    const now = this.now();
    if (report.active) {
      entry.integration = {
        agentId: report.agentId,
        processId: report.processId,
        processKey: reporterKey,
        state: report.state,
        message: report.message,
        seq: report.seq,
        leaseUntil: now + this.leaseMs,
      };
    } else if (entry.integration?.processKey === reporterKey) {
      entry.integration = undefined;
    }
    this.recompute(report.sessionId, now);
    return true;
  }

  reportScreen(sessionId: string, report: ScreenStatusReport): boolean {
    const entry = this.entry(sessionId);
    const observedKey = processKey(report.agentId, report.processId);
    if (entry.retiredProcessKeys.has(observedKey)) {
      if (!entry.screenAbsenceObserved) return false;
      this.reopenProcess(entry, observedKey);
    }
    entry.screenAbsenceObserved = false;
    entry.screen = {
      agentId: report.agentId,
      processId: report.processId,
      processKey: observedKey,
      state: report.state,
    };
    this.recompute(sessionId);
    return true;
  }

  clearScreen(sessionId: string): void {
    const entry = this.entries.get(sessionId);
    if (!entry) return;
    if (entry.screen) {
      entry.screen = undefined;
      this.recompute(sessionId);
    }
    entry.screenAbsenceObserved = true;
  }

  expireLeases(now = this.now()): void {
    for (const [sessionId, entry] of this.entries) {
      if (entry.integration && entry.integration.leaseUntil <= now) {
        this.recompute(sessionId, now);
      }
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
    for (const status of this.snapshot()) this.publish(status);
  }

  snapshot(): AgentStatusUpdateType[] {
    const statuses: AgentStatusUpdateType[] = [];
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
        status_epoch: this.statusEpoch,
        occupant_id: effective.occupantId,
        source: effective.source,
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
