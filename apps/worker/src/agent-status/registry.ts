// Aggregates ancestry-verified integration and screen observations into the
// identified AgentStatusUpdate frames sent to the coordinator. One registry
// owns one worker epoch; each uninterrupted agent-kind/PID incarnation owns
// one occupant token, and an occupant that exits after finishing keeps its
// completion until a viewer acknowledges it. The candidate/occupant shapes it
// drives live in ./occupancy.ts. Process IDs remain private to this module and
// its local producers.
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
import type { AgentProcessIdentity, BuiltinAgentId } from "./process-scan.ts";
import {
  processKey,
  type CandidateLoss,
  type EffectiveEntry,
  type SessionEntry,
} from "./occupancy.ts";

export const INTEGRATION_LEASE_MS = 30_000;

/** Agent runtimes whose integration reports cover the whole agent lifecycle,
 * so no screen signal may correct them. Every other reporter proves identity
 * and activity only: a visible blocker prompt outranks its state. Keyed by
 * agent id because our report protocol carries no source field; the `=== true`
 * test keeps an inherited prototype key such as `constructor` out. */
const FULL_LIFECYCLE_AGENTS: Record<string, true> = { omp: true, pi: true };

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
  visibleBlocker: boolean;
}

/** Exact status fence plus the process identity that proved it. Process IDs
 * never leave the worker; prompt admission compares this proof twice around
 * the shared keeper-input queue. */
export interface AgentStatusPrivateProof {
  statusEpoch: StatusEpoch;
  occupantId: AgentOccupantId;
  revision: number;
  state: AgentRuntimeState;
  source: AgentStatusSource;
  process: AgentProcessIdentity;
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

  private effectiveFrame(
    sessionId: string,
    effective: EffectiveEntry,
    active: boolean,
    revision = effective.revision,
    updatedAt = effective.updatedAt,
  ): AgentStatusUpdateType {
    return AgentStatusUpdate.parse({
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
      occupant_exited: !effective.occupantLive,
    });
  }

  private publishEffective(
    sessionId: string,
    effective: EffectiveEntry,
    active: boolean,
    revision = effective.revision,
    updatedAt = effective.updatedAt,
  ): void {
    this.publish(this.effectiveFrame(sessionId, effective, active, revision, updatedAt));
  }

  private logOccupant(
    event: string,
    sessionId: string,
    occupant: EffectiveEntry,
    revision: number,
  ): void {
    log.info("agent-status", event, {
      session_id: sessionId,
      agent_id: occupant.agentId,
      status_epoch: this.statusEpoch,
      occupant_id: occupant.occupantId,
      source: occupant.source,
      state: occupant.state,
      revision,
    });
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

  private recompute(
    sessionId: string,
    now = this.now(),
    loss: CandidateLoss = "exit",
  ): void {
    const entry = this.entries.get(sessionId);
    if (!entry) return;
    const integration = entry.integration && entry.integration.leaseUntil > now
      ? entry.integration
      : undefined;
    if (entry.integration && !integration) entry.integration = undefined;
    const candidate = integration ?? entry.screen;
    // A screen-visible blocker prompt is direct evidence the agent is waiting
    // on a human, and it outranks a non-authoritative integration that claims
    // otherwise — most often one whose reporter went quiet mid-turn.
    const blockerOverridesIntegration = integration !== undefined
      && FULL_LIFECYCLE_AGENTS[integration.agentId] !== true
      && integration.state !== "blocked"
      && entry.screen?.visibleBlocker === true
      && entry.screen.agentId === integration.agentId;
    const previous = entry.effective;

    if (!candidate) {
      if (!previous || (!previous.occupantLive && loss !== "withdrawn")) return;
      const revision = this.nextRevision(now);
      // An agent that finishes and then leaves is still done. Exit forces the
      // idle transition and keeps the row active so a completion nobody has
      // acknowledged outlives the process that earned it; only an explicit
      // withdrawal or session close retires the occupant here. The row goes out
      // marked `occupant_exited`, because whether the completion still needs
      // showing is known only to a viewer holding its own acknowledgements.
      const retain = loss === "exit"
        && (previous.state !== "idle" || previous.completedRevision > 0);
      const exited: EffectiveEntry = {
        ...previous,
        state: "idle",
        message: undefined,
        occupantLive: false,
        revision,
        completedRevision: previous.state === "idle"
          ? previous.completedRevision
          : revision,
        updatedAt: now,
      };
      entry.effective = retain ? exited : undefined;
      this.publishEffective(sessionId, retain ? exited : previous, retain, revision, now);
      this.logOccupant(
        retain ? "occupant_exited" : "occupant_inactive",
        sessionId,
        retain ? exited : previous,
        revision,
      );
      this.retireProcess(entry, previous.processKey);
      return;
    }

    const source: AgentStatusSource = integration && !blockerOverridesIntegration
      ? "integration"
      : "screen";
    const state: AgentRuntimeState = blockerOverridesIntegration
      ? "blocked"
      : candidate.state;
    const message = integration?.message;
    const sameOccupant = previous?.occupantLive === true
      && previous.processKey === candidate.processKey;
    if (sameOccupant
      && previous.state === state
      && previous.message === message
      && previous.source === source) return;

    if (previous && !sameOccupant) {
      const inactiveRevision = this.nextRevision(now);
      entry.effective = undefined;
      this.publishEffective(sessionId, previous, false, inactiveRevision, now);
      this.logOccupant("occupant_inactive", sessionId, previous, inactiveRevision);
      this.retireProcess(entry, previous.processKey);
    }

    const revision = this.nextRevision(now);
    const completedRevision = sameOccupant
      && previous
      && (previous.state === "working" || previous.state === "blocked")
      && state === "idle"
      ? revision
      : (sameOccupant ? (previous?.completedRevision ?? 0) : 0);
    const effective: EffectiveEntry = {
      agentId: candidate.agentId,
      processId: candidate.processId,
      processKey: candidate.processKey,
      state,
      message,
      source,
      occupantId: sameOccupant && previous
        ? previous.occupantId
        : AgentOccupantId.parse(randomUUID()),
      revision,
      completedRevision,
      updatedAt: now,
      occupantLive: true,
    };
    entry.screenAbsenceObserved = false;
    entry.effective = effective;
    this.publishEffective(sessionId, effective, true);
    this.logOccupant(
      sameOccupant ? "occupant_updated" : "occupant_active",
      sessionId,
      effective,
      effective.revision,
    );
  }

  reportIntegration(report: IntegrationStatusReport): boolean {
    const entry = this.entry(report.sessionId);
    const reporterKey = processKey(report.agentId, report.processId);
    if (entry.retiredProcessKeys.has(reporterKey)) return false;

    const previousSeq = entry.integrationSeqByProcess.get(reporterKey);
    if (previousSeq !== undefined && report.seq <= previousSeq) return false;
    entry.integrationSeqByProcess.set(reporterKey, report.seq);
    const now = this.now();
    let loss: CandidateLoss = "exit";
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
      loss = "withdrawn";
    }
    this.recompute(report.sessionId, now, loss);
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
      visibleBlocker: report.visibleBlocker,
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
    this.recompute(sessionId, this.now(), "withdrawn");
    this.entries.delete(sessionId);
  }

  retainSessions(sessionIds: ReadonlySet<string>): void {
    for (const sessionId of this.entries.keys()) {
      if (!sessionIds.has(sessionId)) this.closeSession(sessionId);
    }
  }

  currentPrivateProof(sessionId: string): AgentStatusPrivateProof | null {
    // A prompt cannot use an integration row during the lease timer's
    // one-second sweep gap, nor the row an exited agent left behind to carry
    // its completion.
    this.recompute(sessionId);
    const effective = this.entries.get(sessionId)?.effective;
    if (!effective || !effective.occupantLive) return null;
    return {
      statusEpoch: this.statusEpoch,
      occupantId: effective.occupantId,
      revision: effective.revision,
      state: effective.state,
      source: effective.source,
      process: {
        agentId: effective.agentId,
        pid: effective.processId,
      },
    };
  }

  resend(): void {
    for (const status of this.snapshot()) this.publish(status);
  }

  snapshot(): AgentStatusUpdateType[] {
    const statuses: AgentStatusUpdateType[] = [];
    for (const [sessionId, entry] of this.entries) {
      const effective = entry.effective;
      if (!effective) continue;
      statuses.push(this.effectiveFrame(sessionId, effective, true));
    }
    return statuses;
  }

  dispose(): void {
    if (this.leaseTimer) clearInterval(this.leaseTimer);
    this.leaseTimer = null;
    this.entries.clear();
  }
}
