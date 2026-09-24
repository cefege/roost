// Coordinator-authorized direct-terminal grants for this browser document.
// State is keyed by worker so loopback and remote peer attempts cannot replace
// each other's credentials. Consumers subscribe to worker-scoped updates; this
// owner fences every asynchronous mint against the current auth generation.

import { diag } from "@roost/observability/diag";
import { TERMINAL_PEER_MAX_SESSIONS_PER_GRANT } from "@roost/protocol/terminal-peer";
import { getCurrentWebKeyInfo } from "../../client/auth/web-key.ts";
import { getTabId } from "../../client/auth/tab-id.ts";
import { coordClient } from "../../client/rpc/connect.ts";
import { rootStore } from "../root.ts";

export const LOCAL_TERMINAL_GRANT_RENEW_MS = 60 * 60_000;
export const LOCAL_TERMINAL_GRANT_RETRY_MS = 30_000;

export interface LocalTerminalGrant {
  readonly workerFp: string;
  readonly grantId: string;
  readonly secret: string;
  readonly sessionIds: readonly string[];
  readonly tabId: string;
  readonly deviceFingerprint: string;
  readonly workerEpoch: string;
  readonly peerSupported: boolean;
  readonly stunUrls: readonly string[];
  readonly inputRouteSupported: boolean;
}

export type TerminalGrantRefreshReason =
  | "demand_added"
  | "renewal"
  | "sync_connected"
  | "door_discovered"
  | "peer_retry";

type TerminalGrantListener = (grant: LocalTerminalGrant | null) => void;

interface WorkerGrantState {
  readonly workerFp: string;
  readonly wantedSessions: Set<string>;
  readonly listeners: Set<TerminalGrantListener>;
  grant: LocalTerminalGrant | null;
  retryAtMs: number;
  demandVersion: number;
  refreshAgain: boolean;
  inFlight: Promise<LocalTerminalGrant | null> | null;
}

/**
 * One document owns every coordinator-minted direct credential. A worker's
 * listeners see only that worker's grant, so a delayed mint cannot redial or
 * replace a carrier for another worker.
 */
export class TerminalGrantOwner {
  private readonly states = new Map<string, WorkerGrantState>();
  private readonly retiredWorkers = new Set<string>();
  private renewalTimer: Timer | null = null;
  current(workerFp: string): LocalTerminalGrant | null {
    return this.states.get(workerFp)?.grant ?? null;
  }

  isWorkerRetired(workerFp: string): boolean {
    return this.retiredWorkers.has(workerFp);
  }

  subscribe(workerFp: string, listener: TerminalGrantListener): () => void {
    if (this.retiredWorkers.has(workerFp)) return () => undefined;
    const state = this.state(workerFp);
    state.listeners.add(listener);
    return () => state.listeners.delete(listener);
  }

  /**
   * Visible demand is scoped to a worker/session pair. Removing demand leaves
   * a currently valid grant intact until normal renewal, but it cannot grow
   * again without a fresh coordinator-authorized mint.
   */
  setDemand(workerFp: string, sessionId: string, active: boolean): void {
    if (!workerFp || !sessionId || this.retiredWorkers.has(workerFp)) return;
    const state = this.state(workerFp);
    if (active) {
      if (state.wantedSessions.has(sessionId)) return;
      state.wantedSessions.add(sessionId);
      state.demandVersion += 1;
      this.armRenewal();
      void this.refresh(workerFp, "demand_added");
      return;
    }
    if (!state.wantedSessions.delete(sessionId)) return;
    state.demandVersion += 1;
    this.disarmRenewalWhenIdle();
  }

  /**
   * Coalesces concurrent mints. If demand expands during the request, the
   * response may cover the original set but a second mint is scheduled before
   * the expanded session can be used.
   */
  refresh(
    workerFp: string,
    reason: TerminalGrantRefreshReason,
  ): Promise<LocalTerminalGrant | null> {
    if (this.retiredWorkers.has(workerFp)) return Promise.resolve(null);
    const state = this.states.get(workerFp);
    if (!state || state.wantedSessions.size === 0) return Promise.resolve(state?.grant ?? null);
    if (state.inFlight) {
      state.refreshAgain = true;
      return state.inFlight;
    }

    const eligible = grantableSessions(
      workerFp,
      state.wantedSessions,
      state.grant?.sessionIds ?? [],
    );
    if (eligible.length === 0) return Promise.resolve(state.grant);
    const needsExpandedGrant = !covers(state.grant, eligible);
    if (reason === "demand_added" && !needsExpandedGrant) return Promise.resolve(state.grant);
    if (needsExpandedGrant && Date.now() < state.retryAtMs) {
      return Promise.resolve(state.grant);
    }

    const capturedDemandVersion = state.demandVersion;
    const capturedAuthGeneration = rootStore.auth_generation;
    const operation = this.mint(workerFp, eligible, capturedAuthGeneration);
    state.inFlight = operation;
    return operation.then((minted) => {
      if (!this.isCurrentState(state, capturedAuthGeneration)) return null;
      if (!minted) {
        state.retryAtMs = Date.now() + LOCAL_TERMINAL_GRANT_RETRY_MS;
        return state.grant;
      }
      state.retryAtMs = 0;
      state.grant = minted;
      this.publish(state, minted);
      return minted;
    }).finally(() => {
      if (state.inFlight !== operation) return;
      state.inFlight = null;
      const refreshAgain = state.refreshAgain || state.demandVersion !== capturedDemandVersion;
      state.refreshAgain = false;
      if (
        refreshAgain
        && this.states.get(workerFp) === state
        && !this.retiredWorkers.has(workerFp)
        && state.wantedSessions.size > 0
      ) {
        queueMicrotask(() => void this.refresh(workerFp, "demand_added"));
      }
    });
  }

  /** The worker rejected a live carrier's credential; only a new mint can recover it. */
  drop(workerFp: string): void {
    const state = this.states.get(workerFp);
    if (!state || state.grant === null) return;
    state.grant = null;
    state.retryAtMs = 0;
    this.publish(state, null);
  }

  retireWorker(workerFp: string): void {
    this.retiredWorkers.add(workerFp);
    const state = this.states.get(workerFp);
    if (!state) {
      this.disarmRenewalWhenIdle();
      return;
    }
    state.grant = null;
    state.wantedSessions.clear();
    state.demandVersion += 1;
    state.refreshAgain = false;
    this.states.delete(workerFp);
    this.publish(state, null);
    this.disarmRenewalWhenIdle();
  }

  clearRetry(workerFp?: string): void {
    if (workerFp !== undefined) {
      const state = this.states.get(workerFp);
      if (state) state.retryAtMs = 0;
      return;
    }
    for (const state of this.states.values()) state.retryAtMs = 0;
  }

  /** Revokes document-held credentials before another authenticated identity can install them. */
  reset(): void {
    for (const state of this.states.values()) {
      state.grant = null;
      state.wantedSessions.clear();
      state.demandVersion += 1;
      state.refreshAgain = false;
      this.publish(state, null);
    }
    this.states.clear();
    this.retiredWorkers.clear();
    this.releaseRenewal();
  }

  releaseRenewal(): void {
    if (this.renewalTimer === null) return;
    clearInterval(this.renewalTimer);
    this.renewalTimer = null;
  }

  private state(workerFp: string): WorkerGrantState {
    let state = this.states.get(workerFp);
    if (state) return state;
    state = {
      workerFp,
      wantedSessions: new Set(),
      listeners: new Set(),
      grant: null,
      retryAtMs: 0,
      demandVersion: 0,
      refreshAgain: false,
      inFlight: null,
    };
    this.states.set(workerFp, state);
    return state;
  }

  private isCurrentState(state: WorkerGrantState, authGeneration: number): boolean {
    return this.states.get(state.workerFp) === state
      && !this.retiredWorkers.has(state.workerFp)
      && rootStore.auth_generation === authGeneration;
  }

  private publish(state: WorkerGrantState, grant: LocalTerminalGrant | null): void {
    for (const listener of state.listeners) listener(grant);
  }

  private armRenewal(): void {
    if (this.renewalTimer !== null) return;
    this.renewalTimer = setInterval(() => {
      for (const state of this.states.values()) {
        if (state.wantedSessions.size > 0) void this.refresh(state.workerFp, "renewal");
      }
    }, LOCAL_TERMINAL_GRANT_RENEW_MS);
  }

  private disarmRenewalWhenIdle(): void {
    for (const state of this.states.values()) {
      if (state.wantedSessions.size > 0) return;
    }
    this.releaseRenewal();
  }

  private async mint(
    workerFp: string,
    sessionIds: readonly string[],
    authGeneration: number,
  ): Promise<LocalTerminalGrant | null> {
    const tabId = getTabId();
    try {
      const deviceFingerprint = (await getCurrentWebKeyInfo()).fingerprint;
      if (rootStore.auth_generation !== authGeneration) return null;
      const response = await coordClient.sessionsGrantLocalTerminal({
        sessionIds: [...sessionIds],
        workerFp,
        tabId,
      });
      if (rootStore.auth_generation !== authGeneration || !response.grantId || !response.secret) {
        return null;
      }
      const grant: LocalTerminalGrant = {
        workerFp,
        grantId: response.grantId,
        secret: response.secret,
        sessionIds: [...sessionIds],
        tabId,
        deviceFingerprint,
        workerEpoch: response.workerEpoch ?? "",
        peerSupported: response.peerSupported === true,
        stunUrls: response.peerSupported === true ? [...(response.stunUrls ?? [])] : [],
        inputRouteSupported: response.inputRouteSupported === true,
      };
      diag("terminal_direct.grant_minted", {
        worker_fp: workerFp,
        sessions: sessionIds.length,
        peer_supported: grant.peerSupported,
      });
      return grant;
    } catch (error) {
      if (rootStore.auth_generation === authGeneration) {
        diag("terminal_direct.grant_refused", {
          worker_fp: workerFp,
          sessions: sessionIds.length,
          error: String(error),
        });
      }
      return null;
    }
  }
}

export const terminalGrantOwner = new TerminalGrantOwner();

export function currentTerminalGrant(workerFp: string): LocalTerminalGrant | null {
  return terminalGrantOwner.current(workerFp);
}

export function subscribeTerminalGrant(
  workerFp: string,
  listener: TerminalGrantListener,
): () => void {
  return terminalGrantOwner.subscribe(workerFp, listener);
}

export function setTerminalGrantDemand(workerFp: string, sessionId: string, active: boolean): void {
  terminalGrantOwner.setDemand(workerFp, sessionId, active);
}

export function refreshTerminalGrant(
  workerFp: string,
  reason: TerminalGrantRefreshReason,
): Promise<LocalTerminalGrant | null> {
  return terminalGrantOwner.refresh(workerFp, reason);
}

export function dropTerminalGrant(workerFp: string): void {
  terminalGrantOwner.drop(workerFp);
}

export function clearTerminalGrantRetry(workerFp?: string): void {
  terminalGrantOwner.clearRetry(workerFp);
}

export function retireTerminalGrantsForWorker(workerFp: string): void { terminalGrantOwner.retireWorker(workerFp); }

/** True after an explicit coordinator-confirmed removal until the auth boundary resets. */
export function isTerminalGrantWorkerRetired(workerFp: string): boolean {
  return terminalGrantOwner.isWorkerRetired(workerFp);
}
export function resetTerminalGrants(): void {
  terminalGrantOwner.reset();
}

export function _releaseTerminalGrantRenewalForTest(): void {
  terminalGrantOwner.releaseRenewal();
}

function grantableSessions(
  workerFp: string,
  wantedSessions: ReadonlySet<string>,
  retainedSessions: readonly string[],
): string[] {
  const granted: string[] = [];
  for (const sessionId of new Set([...retainedSessions, ...wantedSessions])) {
    const session = rootStore.sessions[sessionId];
    if (!session || session.status !== "open" || session.worker_fp !== workerFp) continue;
    granted.push(sessionId);
    if (granted.length === TERMINAL_PEER_MAX_SESSIONS_PER_GRANT) break;
  }
  return granted.sort();
}

function covers(grant: LocalTerminalGrant | null, sessionIds: readonly string[]): boolean {
  return grant !== null && sessionIds.every((sessionId) => grant.sessionIds.includes(sessionId));
}
