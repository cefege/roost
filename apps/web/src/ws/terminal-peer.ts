// Browser-owned direct-peer lifecycle and worker-scoped route ownership.
// It turns visible demand into bounded RTC attempts, accepts loopback candidates,
// and delegates per-session handoff sequencing to terminal-peer-promotions.
// Adapters own packets and registration; this owner owns attempt state and liveness.

import { diag } from "@roost/shared/diag";
import { TERMINAL_PEER_HEARTBEAT_INTERVAL_MS, TERMINAL_PEER_MAX_CONNECTIONS_PER_BROWSER_DOCUMENT, TERMINAL_PEER_NATIVE_ANSWER_DEADLINE_MS, TERMINAL_PEER_NEGOTIATION_DEADLINE_MS } from "@roost/shared/terminal-peer";
import { backoffDelayMs } from "@roost/shared/retry";
import { coordClient } from "../connect.ts";
import { readLocalWorkerDoor, type LocalWorkerDoor } from "../lib/localWorkerDiscovery.ts";
import { terminalDirectRegistry, type TerminalDirectConnection, type TerminalDirectRegistry, type TerminalDirectRegistryEvent } from "../store/terminal-stream-transport.ts";
import { terminalGenerationTokenEquals } from "../store/terminal-stream-types.ts";
import { rootStore } from "../store/root.ts";
import { registerSyncV2ProbeResultHandler } from "../store/sync.ts";
import { LOCAL_TERMINAL_GRANT_RETRY_MS, currentTerminalGrant, dropTerminalGrant, isTerminalGrantWorkerRetired, refreshTerminalGrant, setTerminalGrantDemand, subscribeTerminalGrant, type LocalTerminalGrant } from "./local-terminal-grants.ts";
import { TerminalPeerFallbackClaims } from "./terminal-peer-fallback.ts";
import { TerminalPeerPromotions } from "./terminal-peer-promotions.ts";
import { retireTerminalInput, settleTerminalInput, terminalInputPhase } from "./terminal-input-router.ts";
import { TerminalPeerConnection, type TerminalPeerConnectionDependencies } from "./terminal-peer-connection.ts";
import { activeTerminalPeerViewCount as activeViewCount, beginTerminalPeerHeartbeatEpisode, hasTerminalPeerDemand as hasDemand, recordTerminalPeerViewDemand as recordViewDemand, terminalPeerDeadline as deadline, terminalPeerNow as now, terminalPeerPageHidden as pageHidden } from "./terminal-peer-runtime.ts";
import { createTerminalDirectRequestId } from "./terminal-direct-browser.ts";

const LOOPBACK_GRACE_MS = 2_100, PEER_HOLD_DOWN_MS = 30_000;
const PEER_STABLE_MS = 60_000, PEER_INACTIVE_CLOSE_MS = 30_000;
type PeerPhase = "idle" | "grant" | "gathering" | "negotiating" | "authenticating" | "candidate" | "active" | "cooldown" | "disabled";
type FallbackReason = "unsupported" | "disabled" | "cap" | "network_failed" | "coordinator_unavailable" | "invalid_response" | "ice_failed" | null;
interface WorkerPeerState {
  readonly workerFp: string; readonly demandedSessions: Set<string>; unsubscribeGrant: () => void; grant: LocalTerminalGrant | null;
  connection: TerminalDirectConnection | null; peerUnregister: (() => void) | null; controller: AbortController | null;
  retryTimer: Timer | null; inactiveTimer: Timer | null; heartbeatTimer: Timer | null; heartbeatPending: boolean;
  heartbeatMisses: number; heartbeatEpisode: number; phase: PeerPhase; failureCount: number;
  readonly fallbackClaims: TerminalPeerFallbackClaims; readonly promotions: TerminalPeerPromotions;
  holdDownUntilMs: number; activeSinceMs: number; lastDemandAtMs: number; fallbackReason: FallbackReason;
  lastFailureDetail: string | null; lastFallbackClaimFailure: string | null; disposed: boolean;
}
export interface TerminalPeerOwnerOptions { readonly registry?: TerminalDirectRegistry; readonly connectionDependencies?: TerminalPeerConnectionDependencies; readonly localDoor?: () => LocalWorkerDoor | null; readonly secureContext?: () => boolean; }
export interface TerminalPeerAttemptSnapshot { readonly phase: PeerPhase; readonly fallbackReason: FallbackReason; readonly activeViews: number; readonly hasConnection: boolean; readonly lastFailureDetail: string | null; }
/** Owns all browser-document WebRTC attempts; loopback hands it ready connections to stage. */
export class TerminalPeerOwner {
  private readonly states = new Map<string, WorkerPeerState>();
  private readonly registry: TerminalDirectRegistry;
  private readonly localDoor: () => LocalWorkerDoor | null;
  private readonly secureContext: () => boolean;
  private registryUnsubscribe: (() => void) | null = null;
  private syncProbeUnsubscribe: (() => void) | null = null;
  private started = false;
  constructor(private readonly options: TerminalPeerOwnerOptions = {}) {
    this.registry = options.registry ?? terminalDirectRegistry;
    this.localDoor = options.localDoor ?? readLocalWorkerDoor;
    this.secureContext = options.secureContext ?? (() => globalThis.isSecureContext === true);
  }
  start(): void {
    if (this.started) return;
    this.started = true;
    this.registryUnsubscribe = this.registry.subscribe((event) => this.handleRegistryEvent(event));
    this.syncProbeUnsubscribe = registerSyncV2ProbeResultHandler((result) => {
      const state = this.states.get(result.workerFp), connection = state?.connection, grant = state?.grant;
      if (!connection || connection.workerEpoch === result.workerEpoch) return;
      if (grant?.workerEpoch === connection.workerEpoch) dropTerminalGrant(result.workerFp);
      connection.close("terminal peer worker epoch changed");
    });
    if (typeof document !== "undefined") document.addEventListener("visibilitychange", this.handleVisibilityChange);
    if (typeof window !== "undefined") { window.addEventListener("pagehide", this.handlePageHide); window.addEventListener("pageshow", this.handlePageShow); }
  }
  dispose(reason: string): void {
    this.registryUnsubscribe?.(); this.registryUnsubscribe = null; this.syncProbeUnsubscribe?.(); this.syncProbeUnsubscribe = null;
    if (typeof document !== "undefined") document.removeEventListener("visibilitychange", this.handleVisibilityChange);
    if (typeof window !== "undefined") { window.removeEventListener("pagehide", this.handlePageHide); window.removeEventListener("pageshow", this.handlePageShow); }
    for (const state of this.states.values()) this.disposeState(state, reason);
    this.states.clear(); this.started = false;
  }
  snapshot(workerFp: string): TerminalPeerAttemptSnapshot | null {
    const state = this.states.get(workerFp);
    return state ? { phase: state.phase, fallbackReason: state.fallbackReason, activeViews: activeViewCount(state), hasConnection: state.connection !== null, lastFailureDetail: state.lastFallbackClaimFailure ?? state.lastFailureDetail } : null;
  }
  stage(connection: TerminalDirectConnection): void {
    const token = connection.token();
    if (!token || token.workerFp !== connection.workerFp) return;
    if (isTerminalGrantWorkerRetired(connection.workerFp)) return connection.close("terminal worker was removed");
    const state = this.state(connection.workerFp);
    if (connection.kind === "webrtc" && state.connection !== connection && !this.reservePeerSlot(state)) {
      this.setPhase(state, "cooldown", "cap"); connection.close("terminal peer document capacity reached"); return;
    }
    if (state.connection === null) { state.connection = connection; state.heartbeatPending = false; }
    for (const sessionId of state.demandedSessions) state.promotions.stage(sessionId, connection);
  }
  retire(connection: TerminalDirectConnection, reason: string): void {
    const state = this.states.get(connection.workerFp);
    if (!state || !this.isStateCurrent(state)) return;
    const activeSessions = [...state.demandedSessions].filter((sessionId) => this.registry.activeForSession(sessionId) === connection);
    const replacement = state.connection === connection
      ? this.registry.candidateForWorker(state.workerFp)
      : null;
    state.promotions.retireConnection(connection, reason);
    const token = connection.token(); if (token) retireTerminalInput(token, reason);
    for (const sessionId of activeSessions) state.fallbackClaims.claim(sessionId);
    if (state.connection !== connection) return;
    if (state.phase === "active" || activeSessions.length > 0) state.holdDownUntilMs = Math.max(state.holdDownUntilMs, now() + PEER_HOLD_DOWN_MS);
    state.controller?.abort(); state.controller = null;
    clearTimeout(state.heartbeatTimer ?? undefined); state.heartbeatTimer = null; state.heartbeatPending = false;
    clearTimeout(state.inactiveTimer ?? undefined); state.inactiveTimer = null; state.connection = null;
    if (connection.kind === "webrtc") { state.peerUnregister?.(); state.peerUnregister = null; }
    const replacementToken = replacement?.token();
    if (
      replacement?.kind === "loopback"
      && replacementToken
      && this.registry.targetForToken(replacementToken) === replacement
    ) {
      state.connection = replacement;
      this.activate(state);
      for (const sessionId of state.demandedSessions) this.stageCurrentConnection(state, sessionId);
      return;
    }
    if (activeViewCount(state) > 0 && !this.hasDirectRoute(state)) this.fail(state, reason, ["gathering", "negotiating", "authenticating"].includes(state.phase) ? "network_failed" : "ice_failed");
  }
  private state(workerFp: string): WorkerPeerState {
    const existing = this.states.get(workerFp); if (existing) return existing;
    let state!: WorkerPeerState;
    const fallbackClaims = new TerminalPeerFallbackClaims((reason) => { state.lastFallbackClaimFailure = reason; }, () => this.isStateCurrent(state));
    const promotions = new TerminalPeerPromotions(workerFp, this.registry, fallbackClaims, {
      hasDemand: (sessionId) => hasDemand(state, sessionId), isCurrent: () => this.isStateCurrent(state),
      committed: (connection) => this.promotionCommitted(state, connection), failed: (reason) => { state.lastFailureDetail = reason; },
    });
    state = { workerFp, demandedSessions: new Set(), unsubscribeGrant: () => undefined, grant: currentTerminalGrant(workerFp), connection: null,
      peerUnregister: null, controller: null, retryTimer: null, inactiveTimer: null, heartbeatTimer: null, heartbeatPending: false,
      heartbeatMisses: 0, heartbeatEpisode: 0, phase: "idle", failureCount: 0, fallbackClaims, promotions, holdDownUntilMs: 0,
      activeSinceMs: 0, lastDemandAtMs: now(), fallbackReason: null, lastFailureDetail: null, lastFallbackClaimFailure: null, disposed: false };
    this.states.set(workerFp, state); state.unsubscribeGrant = subscribeTerminalGrant(workerFp, (grant) => this.presentGrant(state, grant));
    return state;
  }
  private presentGrant(state: WorkerPeerState, grant: LocalTerminalGrant | null): void {
    if (!this.isStateCurrent(state)) return;
    state.grant = grant; if (grant && state.retryTimer !== null) { clearTimeout(state.retryTimer); state.retryTimer = null; }
    const connection = state.connection;
    if (connection instanceof TerminalPeerConnection && (!grant || !connection.updateGrant(grant))) return connection.close("terminal peer grant changed");
    if (grant) queueMicrotask(() => {
      if (!this.isStateCurrent(state) || state.grant !== grant) return;
      for (const sessionId of state.demandedSessions) {
        this.stageCurrentConnection(state, sessionId);
        if (terminalInputPhase(sessionId) === "blocked" && this.registry.activeForSession(sessionId) === null) state.fallbackClaims.claim(sessionId);
      }
    });
    void this.maybeStart(state);
  }
  private handleRegistryEvent(event: TerminalDirectRegistryEvent): void {
    if (event.kind === "worker_retired") {
      const state = this.states.get(event.workerFp); if (state) { this.states.delete(event.workerFp); this.disposeState(state, event.reason); } return;
    }
    if (event.kind === "demand_changed") return this.handleDemand(event);
    const state = event.token.workerFp ? this.states.get(event.token.workerFp) : null;
    if (!state || !this.isStateCurrent(state)) return;
    if (event.kind === "promotion_committed") {
      state.fallbackClaims.retire(event.sessionId);
      if (state.connection && terminalGenerationTokenEquals(state.connection.token(), event.token)) this.activate(state);
      return;
    }
    if (!hasDemand(state, event.sessionId)) return;
    this.stageCurrentConnection(state, event.sessionId);
    if (this.registry.activeForSession(event.sessionId) === null) {
      state.lastFallbackClaimFailure = "terminal Sync fallback input route is pending"; state.fallbackClaims.claim(event.sessionId);
      if (state.phase !== "cooldown" && !this.hasDirectRoute(state)) this.fail(state, event.reason, "ice_failed");
    }
  }
  private handleDemand(event: Extract<TerminalDirectRegistryEvent, { kind: "demand_changed" }>): void {
    if (isTerminalGrantWorkerRetired(event.workerFp)) return;
    const state = this.state(event.workerFp), active = this.registry.hasViewDemand(event.workerFp, event.sessionId);
    const sessionChanged = recordViewDemand(state, event.sessionId, active);
    if (!sessionChanged && !active) return;
    if (sessionChanged) { setTerminalGrantDemand(event.workerFp, event.sessionId, active); state.lastDemandAtMs = now(); }
    if (active) {
      if (state.inactiveTimer !== null) { clearTimeout(state.inactiveTimer); state.inactiveTimer = null; }
      if (sessionChanged && state.connection?.kind === "webrtc") this.heartbeat(state, true, true);
      this.stageCurrentConnection(state, event.sessionId);
      void this.maybeStart(state); return;
    }
    if (activeViewCount(state) === 0) this.armInactiveClose(state);
  }
  private stageCurrentConnection(state: WorkerPeerState, sessionId: string): void {
    const candidate = this.registry.candidateForWorker(state.workerFp);
    const connection = candidate?.kind === "loopback"
      && candidate.token()
      && candidate.allowsSession(sessionId)
      ? candidate
      : state.connection;
    if (!connection) return;
    const active = this.registry.activeForSession(sessionId);
    if (
      active === connection
      || (active?.kind === "loopback" && connection.kind === "webrtc")
    ) return;
    state.promotions.stage(sessionId, connection);
  }
  private async maybeStart(state: WorkerPeerState): Promise<void> {
    if (!this.isStateCurrent(state) || isTerminalGrantWorkerRetired(state.workerFp) || activeViewCount(state) === 0 || state.connection || state.controller || state.retryTimer !== null || pageHidden()) return;
    if (this.localDoor()?.workerFingerprint === state.workerFp) { this.setPhase(state, "idle", null); return this.schedule(state, LOOPBACK_GRACE_MS); }
    if (!this.secureContext() || typeof RTCPeerConnection === "undefined") return this.setPhase(state, "disabled", "unsupported");
    const grant = state.grant;
    if (!grant) { this.setPhase(state, "grant", null); void refreshTerminalGrant(state.workerFp, "peer_retry"); return this.schedule(state, LOCAL_TERMINAL_GRANT_RETRY_MS); }
    if (!grant.peerSupported || !grant.workerEpoch) return this.setPhase(state, "disabled", "disabled");
    if (state.holdDownUntilMs > now()) return this.schedule(state, state.holdDownUntilMs - now());
    if (!this.reservePeerSlot(state)) return this.setPhase(state, "cooldown", "cap");
    await this.negotiate(state, grant);
  }
  private async negotiate(state: WorkerPeerState, grant: LocalTerminalGrant): Promise<void> {
    const controller = new AbortController(), authGeneration = rootStore.auth_generation, peerId = createTerminalDirectRequestId();
    state.controller = controller; this.setPhase(state, "gathering", null); let connection: TerminalPeerConnection | null = null;
    try {
      connection = TerminalPeerConnection.create({ workerFp: state.workerFp, workerEpoch: grant.workerEpoch, peerId, grant, stunUrls: grant.stunUrls,
        hooks: {
          onReady: (ready) => this.peerReady(state, ready, controller, authGeneration), onClosed: (closed, reason) => this.peerClosed(state, closed, reason),
          onInputResult: (token, frame) => settleTerminalInput(token, { sessionId: frame.value.sessionId, inputSeq: frame.value.inputSeq,
            status: frame.case === "inputAccepted" ? "accepted" : frame.case === "inputRejected" ? "rejected" : "ambiguous", writtenBytes: frame.case === "inputRejected" ? undefined : frame.value.writtenBytes, reason: frame.case === "inputAccepted" ? undefined : frame.value.reason }),
        },
      }, this.options.connectionDependencies);
      state.connection = connection;
      const offerSdp = await deadline(connection.createOffer(), TERMINAL_PEER_NEGOTIATION_DEADLINE_MS, controller);
      if (!this.current(state, connection, controller, authGeneration)) return connection.close("terminal peer auth state changed");
      this.setPhase(state, "negotiating", null);
      const response = await deadline(coordClient.sessionsNegotiateLocalTerminalPeer({ workerFp: state.workerFp, grantId: grant.grantId, tabId: grant.tabId, peerId, offerSdp, workerEpoch: grant.workerEpoch }, { signal: controller.signal }), TERMINAL_PEER_NATIVE_ANSWER_DEADLINE_MS, controller);
      if (!this.current(state, connection, controller, authGeneration)) return connection.close("terminal peer auth state changed");
      if (response.peerId !== peerId || response.workerEpoch !== grant.workerEpoch || !response.answerSdp) throw new Error("invalid peer negotiation response");
      this.setPhase(state, "authenticating", null); await deadline(connection.acceptAnswer(response.answerSdp), TERMINAL_PEER_NATIVE_ANSWER_DEADLINE_MS, controller);
    } catch (error) {
      if (state.connection !== connection) return;
      if (error instanceof Error && error.message.includes("terminal peer grant is unavailable") && state.grant === grant) dropTerminalGrant(state.workerFp);
      if (connection) connection.close(`terminal peer negotiation failed: ${error instanceof Error ? error.message : String(error)}`);
      else this.fail(state, String(error), "network_failed");
    } finally { if (state.controller === controller && (connection === null || controller.signal.aborted)) state.controller = null; }
  }
  private peerReady(state: WorkerPeerState, connection: TerminalPeerConnection, controller: AbortController, authGeneration: number): void {
    if (!this.current(state, connection, controller, authGeneration)) return connection.close("terminal peer attempt was superseded");
    state.controller = null; state.peerUnregister = this.registry.register(connection); this.setPhase(state, "candidate", null); this.stage(connection);
  }
  private peerClosed(state: WorkerPeerState, connection: TerminalPeerConnection, reason: string): void {
    if (state.connection !== connection || !this.isStateCurrent(state)) return;
    state.lastFailureDetail = reason; this.retire(connection, reason);
  }
  private promotionCommitted(state: WorkerPeerState, connection: TerminalDirectConnection): void {
    if (!this.isStateCurrent(state)) return;
    const previous = state.connection;
    if (previous === connection) return this.activate(state);
    if (connection.kind === "loopback" && previous?.kind === "webrtc" && !this.registry.hasRoutesForConnection(previous)) {
      previous.close("loopback direct connection replaced peer");
      if (!this.isStateCurrent(state) || state.connection !== null) return;
      state.connection = connection; this.activate(state); return;
    }
    if (previous === null && connection.kind === "loopback") { state.connection = connection; this.activate(state); }
  }
  private activate(state: WorkerPeerState): void {
    state.activeSinceMs = now(); state.heartbeatMisses = 0; state.lastFailureDetail = null; this.setPhase(state, "active", null);
    if (state.connection?.kind === "webrtc") this.heartbeat(state, true, true);
  }
  private heartbeat(state: WorkerPeerState, immediate: boolean, requireFresh = false): void {
    const connection = state.connection;
    if (!this.isStateCurrent(state) || state.phase !== "active" || connection?.kind !== "webrtc" || activeViewCount(state) === 0 || pageHidden()) return;
    if (!immediate) {
      if (state.heartbeatTimer !== null) return;
      state.heartbeatTimer = setTimeout(() => { state.heartbeatTimer = null; this.heartbeat(state, true); }, TERMINAL_PEER_HEARTBEAT_INTERVAL_MS); return;
    }
    const heartbeatEpisode = beginTerminalPeerHeartbeatEpisode(state, connection instanceof TerminalPeerConnection ? connection : null, requireFresh);
    if (heartbeatEpisode === null) return;
    void connection.probe(createTerminalDirectRequestId()).then(() => {
      if (state.connection !== connection || state.heartbeatEpisode !== heartbeatEpisode) return;
      state.heartbeatMisses = 0; if (now() - state.activeSinceMs >= PEER_STABLE_MS) state.failureCount = 0;
    }).catch(() => {
      if (state.connection !== connection || state.heartbeatEpisode !== heartbeatEpisode) return;
      state.heartbeatMisses += 1; if (state.heartbeatMisses >= 2) connection.close("terminal peer heartbeat missed");
    }).finally(() => {
      if (state.connection !== connection || state.heartbeatEpisode !== heartbeatEpisode) return;
      state.heartbeatPending = false; this.heartbeat(state, false);
    });
  }
  private fail(state: WorkerPeerState, reason: string, fallbackReason: Exclude<FallbackReason, null>): void {
    if (!this.isStateCurrent(state)) return;
    if (state.phase === "active") state.holdDownUntilMs = now() + PEER_HOLD_DOWN_MS;
    state.failureCount += 1; this.setPhase(state, "cooldown", fallbackReason);
    this.schedule(state, state.holdDownUntilMs > now() ? state.holdDownUntilMs - now() : backoffDelayMs(state.failureCount - 1, { baseMs: 1_000, maxMs: 30_000 }));
    diag("terminal_peer.failed", { worker_fp: state.workerFp, reason: fallbackReason, detail: reason });
  }
  private schedule(state: WorkerPeerState, delayMs: number): void {
    if (state.retryTimer !== null) return;
    state.retryTimer = setTimeout(() => { state.retryTimer = null; void this.maybeStart(state); }, Math.max(0, delayMs));
  }
  private reservePeerSlot(state: WorkerPeerState): boolean {
    const occupied = [...this.states.values()].filter((item) => item.connection?.kind === "webrtc");
    if (occupied.length < TERMINAL_PEER_MAX_CONNECTIONS_PER_BROWSER_DOCUMENT) return true;
    const inactive = occupied.filter((item) => item !== state && activeViewCount(item) === 0).sort((left, right) => left.lastDemandAtMs - right.lastDemandAtMs)[0];
    if (!inactive) return false;
    inactive.connection?.close("terminal peer document capacity evicted inactive worker"); return true;
  }
  private armInactiveClose(state: WorkerPeerState): void {
    if (state.connection?.kind !== "webrtc" || state.inactiveTimer !== null) return;
    state.inactiveTimer = setTimeout(() => { state.inactiveTimer = null; if (activeViewCount(state) === 0) state.connection?.close("terminal peer became inactive"); }, PEER_INACTIVE_CLOSE_MS);
  }
  private current(state: WorkerPeerState, connection: TerminalDirectConnection, controller: AbortController, authGeneration: number): boolean {
    return this.isStateCurrent(state) && state.connection === connection && state.controller === controller && !controller.signal.aborted && rootStore.auth_generation === authGeneration && activeViewCount(state) > 0;
  }
  private isStateCurrent(state: WorkerPeerState): boolean { return !state.disposed && this.states.get(state.workerFp) === state && !isTerminalGrantWorkerRetired(state.workerFp); }
  private hasDirectRoute(state: WorkerPeerState): boolean { for (const sessionId of state.demandedSessions) if (this.registry.activeForSession(sessionId)) return true; return false; }
  private setPhase(state: WorkerPeerState, phase: PeerPhase, fallbackReason: FallbackReason): void {
    if (state.phase === phase && state.fallbackReason === fallbackReason) return;
    state.phase = phase; state.fallbackReason = fallbackReason; diag("terminal_peer.phase", { worker_fp: state.workerFp, phase, fallback_reason: fallbackReason });
  }
  private disposeState(state: WorkerPeerState, reason: string): void {
    if (state.disposed) return;
    state.disposed = true; state.controller?.abort(); state.controller = null;
    if (state.retryTimer !== null) clearTimeout(state.retryTimer);
    if (state.inactiveTimer !== null) clearTimeout(state.inactiveTimer);
    if (state.heartbeatTimer !== null) clearTimeout(state.heartbeatTimer);
    state.retryTimer = null; state.inactiveTimer = null; state.heartbeatTimer = null;
    state.promotions.dispose(reason); state.fallbackClaims.dispose(); state.unsubscribeGrant(); state.peerUnregister?.(); state.peerUnregister = null;
    if (state.connection?.kind === "webrtc") state.connection.close(reason);
    state.connection = null;
  }
  private readonly handleVisibilityChange = (): void => { for (const state of this.states.values()) { if (pageHidden()) this.armInactiveClose(state); else { this.heartbeat(state, true, true); void this.maybeStart(state); } } };
  private readonly handlePageHide = (): void => { for (const state of this.states.values()) if (state.connection?.kind === "webrtc") state.connection.close("terminal peer page hidden"); };
  private readonly handlePageShow = (): void => { for (const state of this.states.values()) void this.maybeStart(state); };
}

const documentTerminalPeerOwner = new TerminalPeerOwner();
export function startTerminalPeerFastPath(): void { documentTerminalPeerOwner.start(); }
export function resetTerminalPeerState(reason: string): void { documentTerminalPeerOwner.dispose(reason); }
export function stageTerminalDirectConnection(connection: TerminalDirectConnection): void { documentTerminalPeerOwner.stage(connection); }
export function retireTerminalDirectConnection(connection: TerminalDirectConnection, reason: string): void { documentTerminalPeerOwner.retire(connection, reason); }
export function terminalPeerAttemptSnapshot(workerFp: string): TerminalPeerAttemptSnapshot | null { return documentTerminalPeerOwner.snapshot(workerFp); }
