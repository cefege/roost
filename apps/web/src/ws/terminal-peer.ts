// Browser-owned direct-peer election and promotion orchestration.
// It converts visible worker demand into a bounded RTC candidate, stages frames
// before route election, and coordinates input drain/claim/commit transitions.
// The adapter owns WebRTC packets; this owner never imports a worker-native module.

import { diag } from "@roost/shared/diag";
import {
  TERMINAL_PEER_MAX_CONNECTIONS_PER_BROWSER_DOCUMENT,
  TERMINAL_PEER_HEARTBEAT_INTERVAL_MS,
  TERMINAL_PEER_NEGOTIATION_DEADLINE_MS,
  TERMINAL_PEER_NATIVE_ANSWER_DEADLINE_MS,
} from "@roost/shared/terminal-peer";
import { backoffDelayMs } from "@roost/shared/retry";
import { coordClient } from "../connect.ts";
import { readLocalWorkerDoor, type LocalWorkerDoor } from "../lib/localWorkerDiscovery.ts";
import { currentTerminalGenerationToken } from "../store/terminal-stream-publication.ts";
import { createTerminalSessionPromotion, type TerminalSessionPromotion } from "../store/terminal-stream-promotion.ts";
import { terminalDirectRegistry, type TerminalDirectConnection, type TerminalDirectRegistry, type TerminalDirectRegistryEvent } from "../store/terminal-stream-transport.ts";
import { terminalGenerationTokenEquals, type TerminalGenerationToken } from "../store/terminal-stream-types.ts";
import { rootStore } from "../store/root.ts";
import { registerSyncV2ProbeResultHandler } from "../store/sync.ts";
import { LOCAL_TERMINAL_GRANT_RETRY_MS, currentTerminalGrant, dropTerminalGrant, refreshTerminalGrant, setTerminalGrantDemand, subscribeTerminalGrant, type LocalTerminalGrant } from "./local-terminal-grants.ts";
import { TerminalPeerFallbackClaims } from "./terminal-peer-fallback.ts";
import { claimTerminalInputRoute, drainTerminalInput, holdTerminalInput, retireTerminalInput, settleTerminalInput, type TerminalInputDestination, type TerminalInputHoldRelease } from "./terminal-input-router.ts";
import { terminalInputDestinationForDirectConnection, terminalInputDestinationForSession } from "./sync-outbound.ts";
import { TerminalPeerConnection, type TerminalPeerConnectionDependencies } from "./terminal-peer-connection.ts";
import {
  activeTerminalPeerViewCount as activeViewCount,
  beginTerminalPeerHeartbeatEpisode,
  hasTerminalPeerDemand as hasDemand,
  recordTerminalPeerViewDemand as recordViewDemand,
  terminalPeerDeadline as deadline,
  terminalPeerNow as now,
  terminalPeerPageHidden as pageHidden,
} from "./terminal-peer-runtime.ts";
import { createTerminalDirectRequestId } from "./terminal-direct-browser.ts";
const LOOPBACK_GRACE_MS = 2_100; const PEER_HOLD_DOWN_MS = 30_000;
const PEER_STABLE_MS = 60_000;
const PEER_INACTIVE_CLOSE_MS = 30_000;
type PeerPhase = "idle" | "grant" | "gathering" | "negotiating" | "authenticating" | "candidate" | "active" | "cooldown" | "disabled";
type FallbackReason = "unsupported" | "disabled" | "cap" | "network_failed" | "coordinator_unavailable" | "invalid_response" | "ice_failed" | null;
interface PromotionRun { readonly candidate: TerminalSessionPromotion; readonly connection: TerminalDirectConnection;
  readonly oldToken: TerminalGenerationToken | null; readonly oldDestination: TerminalInputDestination | null;
  release: TerminalInputHoldRelease | null; claimMayHaveReached: boolean; promoting: boolean; committed: boolean;
}
interface WorkerPeerState {
  readonly workerFp: string; readonly demandedSessions: Set<string>; readonly promotions: Map<string, PromotionRun>;
  readonly unsubscribeGrant: () => void; grant: LocalTerminalGrant | null; connection: TerminalDirectConnection | null;
  unregister: (() => void) | null; controller: AbortController | null; retryTimer: Timer | null; inactiveTimer: Timer | null;
  heartbeatTimer: Timer | null; heartbeatPending: boolean; heartbeatMisses: number; heartbeatEpisode: number; phase: PeerPhase; failureCount: number;
  readonly fallbackClaims: TerminalPeerFallbackClaims; holdDownUntilMs: number; activeSinceMs: number; lastDemandAtMs: number; fallbackReason: FallbackReason; lastFailureDetail: string | null; lastFallbackClaimFailure: string | null;
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
      const state = this.states.get(result.workerFp);
      if (state?.connection && state.connection.workerEpoch !== result.workerEpoch) {
        if (state.grant?.workerEpoch === state.connection.workerEpoch) dropTerminalGrant(result.workerFp); state.connection.close("terminal peer worker epoch changed");
      }
    });
    if (typeof document !== "undefined") document.addEventListener("visibilitychange", this.handleVisibilityChange);
    if (typeof window !== "undefined") { window.addEventListener("pagehide", this.handlePageHide); window.addEventListener("pageshow", this.handlePageShow); }
  }
  dispose(reason: string): void {
    this.registryUnsubscribe?.(); this.registryUnsubscribe = null;
    this.syncProbeUnsubscribe?.(); this.syncProbeUnsubscribe = null;
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
    const state = this.state(connection.workerFp);
    if (connection.kind === "webrtc" && state.connection !== connection && !this.reservePeerSlot(state)) {
      this.setPhase(state, "cooldown", "cap");
      connection.close("terminal peer document capacity reached");
      return;
    }
    if (state.connection && state.connection !== connection && state.connection.kind === "webrtc") state.connection.close("loopback direct connection became available");
    if (state.connection !== connection) state.heartbeatPending = false; state.connection = connection;
    for (const sessionId of state.demandedSessions) this.stageSession(state, sessionId, connection);
  }
  retire(connection: TerminalDirectConnection, reason: string): void {
    const state = this.states.get(connection.workerFp);
    if (!state || state.connection !== connection) return;
    const token = connection.token();
    let hadActiveRoute = state.phase === "active";
    for (const sessionId of state.demandedSessions) {
      if (this.registry.activeForSession(sessionId) === connection) hadActiveRoute = true;
    }
    if (hadActiveRoute) state.holdDownUntilMs = Math.max(state.holdDownUntilMs, now() + PEER_HOLD_DOWN_MS);
    state.controller?.abort(); state.controller = null;
    clearTimeout(state.heartbeatTimer ?? undefined); state.heartbeatTimer = null; state.heartbeatPending = false;
    clearTimeout(state.inactiveTimer ?? undefined); state.inactiveTimer = null;
    state.connection = null;
    if (token) retireTerminalInput(token, reason);
    state.unregister?.(); state.unregister = null;
    for (const sessionId of state.demandedSessions) state.fallbackClaims.claim(sessionId);
    this.cancelPromotions(state, reason);
    if (activeViewCount(state) > 0) this.fail(state, reason, ["gathering", "negotiating", "authenticating"].includes(state.phase) ? "network_failed" : "ice_failed");
  }
  private state(workerFp: string): WorkerPeerState {
    const existing = this.states.get(workerFp);
    if (existing) return existing;
    let state!: WorkerPeerState;
    const unsubscribeGrant = subscribeTerminalGrant(workerFp, (grant) => {
      state.grant = grant; if (grant && state.retryTimer !== null) { clearTimeout(state.retryTimer); state.retryTimer = null; }
      const connection = state.connection;
      if (connection instanceof TerminalPeerConnection && (!grant || !connection.updateGrant(grant))) return connection.close("terminal peer grant changed");
      if (connection && grant) queueMicrotask(() => {
        if (state.grant !== grant || state.connection !== connection) return;
        for (const sessionId of state.demandedSessions) {
          if (this.registry.activeForSession(sessionId) !== connection) this.stageSession(state, sessionId, connection);
        }
      });
      void this.maybeStart(state);
    });
    state = {
      workerFp, demandedSessions: new Set(), promotions: new Map(), unsubscribeGrant, grant: currentTerminalGrant(workerFp), connection: null,
      unregister: null, controller: null, retryTimer: null, inactiveTimer: null, heartbeatTimer: null, heartbeatPending: false,
      heartbeatMisses: 0, heartbeatEpisode: 0, phase: "idle", failureCount: 0, holdDownUntilMs: 0, activeSinceMs: 0, lastDemandAtMs: now(), fallbackReason: null, lastFailureDetail: null, lastFallbackClaimFailure: null, fallbackClaims: new TerminalPeerFallbackClaims((reason) => { state.lastFallbackClaimFailure = reason; }),
    };
    this.states.set(workerFp, state);
    return state;
  }
  private handleRegistryEvent(event: TerminalDirectRegistryEvent): void {
    if (event.kind === "worker_retired") {
      const state = this.states.get(event.workerFp); if (state) { this.states.delete(event.workerFp); this.disposeState(state, event.reason); } return;
    }
    if (event.kind === "demand_changed") return this.handleDemand(event);
    const state = event.token.workerFp ? this.states.get(event.token.workerFp) : null;
    if (!state) return;
    if (event.kind === "promotion_committed") {
      if (state.connection && terminalGenerationTokenEquals(state.connection.token(), event.token)) {
        state.fallbackClaims.retire(event.sessionId); this.activate(state);
      }
      return;
    }
    const connectionIsCurrent = state.connection !== null && terminalGenerationTokenEquals(state.connection.token(), event.token);
    if (connectionIsCurrent) {
      const connection = state.connection!;
      queueMicrotask(() => { if (state.connection === connection && hasDemand(state, event.sessionId)) this.stageSession(state, event.sessionId, connection); });
      return;
    }
    if (state.connection === null && hasDemand(state, event.sessionId)) {
      state.lastFallbackClaimFailure = "terminal Sync fallback input route is pending"; state.fallbackClaims.claim(event.sessionId);
      if (state.phase !== "cooldown") this.fail(state, event.reason, "ice_failed");
    }
  }
  private handleDemand(event: Extract<TerminalDirectRegistryEvent, { kind: "demand_changed" }>): void {
    const state = this.state(event.workerFp);
    const active = this.registry.hasViewDemand(event.workerFp, event.sessionId);
    const sessionChanged = recordViewDemand(state, event.sessionId, active);
    if (!sessionChanged && !active) return;
    if (sessionChanged) { setTerminalGrantDemand(event.workerFp, event.sessionId, active); state.lastDemandAtMs = now(); }
    if (active) {
      if (state.inactiveTimer !== null) { clearTimeout(state.inactiveTimer); state.inactiveTimer = null; }
      if (sessionChanged && state.connection?.kind === "webrtc") this.heartbeat(state, true, true);
      if (state.connection && this.registry.activeForSession(event.sessionId) !== state.connection) this.stageSession(state, event.sessionId, state.connection);
      void this.maybeStart(state);
      return;
    }
    if (activeViewCount(state) === 0) this.armInactiveClose(state);
  }
  private async maybeStart(state: WorkerPeerState): Promise<void> {
    if (activeViewCount(state) === 0 || state.connection || state.controller || state.retryTimer !== null) return;
    if (pageHidden()) return;
    if (this.localDoor()?.workerFingerprint === state.workerFp) { this.setPhase(state, "idle", null); return this.schedule(state, LOOPBACK_GRACE_MS); }
    if (!this.secureContext() || typeof RTCPeerConnection === "undefined") return this.setPhase(state, "disabled", "unsupported");
    const grant = state.grant;
    if (!grant) { this.setPhase(state, "grant", null); void refreshTerminalGrant(state.workerFp, "peer_retry"); this.schedule(state, LOCAL_TERMINAL_GRANT_RETRY_MS); return; }
    if (!grant.peerSupported || !grant.workerEpoch) return this.setPhase(state, "disabled", "disabled");
    if (state.holdDownUntilMs > now()) return this.schedule(state, state.holdDownUntilMs - now());
    if (!this.reservePeerSlot(state)) return this.setPhase(state, "cooldown", "cap");
    await this.negotiate(state, grant);
  }
  private async negotiate(state: WorkerPeerState, grant: LocalTerminalGrant): Promise<void> {
    const controller = new AbortController();
    const authGeneration = rootStore.auth_generation;
    const peerId = createTerminalDirectRequestId();
    state.controller = controller; this.setPhase(state, "gathering", null);
    let connection: TerminalPeerConnection | null = null;
    try {
      connection = TerminalPeerConnection.create({
        workerFp: state.workerFp, workerEpoch: grant.workerEpoch, peerId, grant, stunUrls: grant.stunUrls,
        hooks: {
          onReady: (ready) => this.peerReady(state, ready, controller, authGeneration),
          onClosed: (closed, reason) => this.peerClosed(state, closed, reason),
          onInputResult: (token, frame) => settleTerminalInput(token, {
            sessionId: frame.value.sessionId, inputSeq: frame.value.inputSeq,
            status: frame.case === "inputAccepted" ? "accepted" : frame.case === "inputRejected" ? "rejected" : "ambiguous",
            writtenBytes: frame.case === "inputRejected" ? undefined : frame.value.writtenBytes,
            reason: frame.case === "inputAccepted" ? undefined : frame.value.reason,
          }),
        },
      }, this.options.connectionDependencies);
      state.connection = connection;
      const offerSdp = await deadline(connection.createOffer(), TERMINAL_PEER_NEGOTIATION_DEADLINE_MS, controller);
      if (!this.current(state, connection, controller, authGeneration)) return connection.close("terminal peer auth state changed");
      this.setPhase(state, "negotiating", null);
      const response = await deadline(coordClient.sessionsNegotiateLocalTerminalPeer({
        workerFp: state.workerFp, grantId: grant.grantId, tabId: grant.tabId, peerId, offerSdp, workerEpoch: grant.workerEpoch,
      }, { signal: controller.signal }), TERMINAL_PEER_NATIVE_ANSWER_DEADLINE_MS, controller);
      if (!this.current(state, connection, controller, authGeneration)) return connection.close("terminal peer auth state changed");
      if (response.peerId !== peerId || response.workerEpoch !== grant.workerEpoch || !response.answerSdp) throw new Error("invalid peer negotiation response");
      this.setPhase(state, "authenticating", null);
      await deadline(connection.acceptAnswer(response.answerSdp), TERMINAL_PEER_NATIVE_ANSWER_DEADLINE_MS, controller);
    } catch (error) {
      if (state.connection !== connection) return; if (error instanceof Error && error.message.includes("terminal peer grant is unavailable") && state.grant === grant) dropTerminalGrant(state.workerFp);
      if (connection) connection.close(`terminal peer negotiation failed: ${error instanceof Error ? error.message : String(error)}`);
      else this.fail(state, String(error), "network_failed");
    } finally { if (state.controller === controller && (connection === null || controller.signal.aborted)) state.controller = null; }
  }
  private peerReady(state: WorkerPeerState, connection: TerminalPeerConnection, controller: AbortController, authGeneration: number): void {
    if (!this.current(state, connection, controller, authGeneration)) return connection.close("terminal peer attempt was superseded");
    state.controller = null; state.unregister = this.registry.register(connection);
    this.setPhase(state, "candidate", null);
    this.stage(connection);
  }
  private peerClosed(state: WorkerPeerState, connection: TerminalPeerConnection, reason: string): void { state.lastFailureDetail = reason;
    if (state.connection === connection) this.retire(connection, reason);
  }
  private stageSession(state: WorkerPeerState, sessionId: string, connection: TerminalDirectConnection): void {
    const token = connection.token();
    if (!token || !connection.allowsSession(sessionId) || !hasDemand(state, sessionId)) return;
    this.cancelPromotion(state, sessionId, "candidate replaced");
    const oldToken = currentTerminalGenerationToken(sessionId);
    const oldDestination = terminalInputDestinationForSession(sessionId);
    const attemptId = createTerminalDirectRequestId();
    const candidate = createTerminalSessionPromotion({ sessionId, attemptId, connection, token,
      onCancelled: (reason) => { state.lastFailureDetail = reason; this.candidateCancelled(state, sessionId, attemptId, reason); } });
    if (!candidate) return;
    const run: PromotionRun = { candidate, connection, oldToken, oldDestination, release: null, claimMayHaveReached: false, promoting: false, committed: false };
    state.promotions.set(sessionId, run);
    void candidate.awaitReady().then((ready) => { if (ready) void this.promote(state, sessionId, run); }).catch(() => this.cancelPromotion(state, sessionId, "candidate readiness failed", run));
  }
  private async promote(state: WorkerPeerState, sessionId: string, run: PromotionRun): Promise<void> {
    if (run.promoting || !run.candidate.isReady() || state.promotions.get(sessionId) !== run || !hasDemand(state, sessionId)) return;
    run.promoting = true; run.release = holdTerminalInput(sessionId);
    try {
      const destination = terminalInputDestinationForDirectConnection(run.connection, true);
      if (!destination) throw new Error("candidate direct connection lost its token");
      if (run.connection.kind === "webrtc" && !destination.inputRouteSupported) throw new Error("terminal peer input-route capability is unavailable");
      let claimEpoch = "";
      if (destination.inputRouteSupported) {
        run.claimMayHaveReached = true;
        const claim = await claimTerminalInputRoute(sessionId, destination);
        if (!claim.accepted) throw new Error(claim.reason);
        claimEpoch = claim.inputRouteEpoch;
      }
      if (run.oldToken && !destination.inputRouteSupported && !terminalGenerationTokenEquals(run.oldToken, run.connection.token())) await deadline(drainTerminalInput(sessionId, run.oldToken), 10_000);
      const prepared = run.candidate.prepare(claimEpoch, run.oldToken);
      if (!prepared || !this.registry.commitSessionPromotion(sessionId, run.candidate.attemptId, prepared)) throw new Error("candidate promotion was superseded");
      run.committed = true; state.promotions.delete(sessionId); this.release(run, destination); this.activate(state);
    } catch (error) { this.cancelPromotion(state, sessionId, String(error), run); }
    finally { run.promoting = false; }
  }
  private candidateCancelled(state: WorkerPeerState, sessionId: string, attemptId: string, reason: string): void {
    const run = state.promotions.get(sessionId);
    if (!run || run.candidate.attemptId !== attemptId || run.committed) return;
    state.promotions.delete(sessionId); state.lastFailureDetail = reason;
    if (run.claimMayHaveReached) void this.recoverOldRoute(sessionId, run); else this.release(run, run.oldDestination);
    diag("terminal_peer.candidate_cancelled", { worker_fp: state.workerFp, reason });
  }
  private cancelPromotion(state: WorkerPeerState, sessionId: string, reason: string, expected?: PromotionRun): void {
    const run = state.promotions.get(sessionId);
    if (!run || (expected && run !== expected)) return;
    state.promotions.delete(sessionId); run.candidate.cancel(reason);
    if (run.claimMayHaveReached) void this.recoverOldRoute(sessionId, run); else this.release(run, run.oldDestination);
  }
  private cancelPromotions(state: WorkerPeerState, reason: string): void {
    for (const sessionId of [...state.promotions.keys()]) this.cancelPromotion(state, sessionId, reason);
  }
  private async recoverOldRoute(sessionId: string, run: PromotionRun): Promise<void> {
    const destination = run.oldDestination;
    if (!destination || !destination.inputRouteSupported || !terminalGenerationTokenEquals(currentTerminalGenerationToken(sessionId), destination.token)) return;
    try {
      const recovered = await claimTerminalInputRoute(sessionId, destination);
      if (recovered.accepted) this.release(run, destination);
    } catch { /* Held input remains blocked when old-route recovery is unconfirmed. */ }
  }
  private release(run: PromotionRun, destination: TerminalInputDestination | null): void {
    if (!run.release) return;
    run.release(destination ?? undefined); run.release = null;
  }
  private activate(state: WorkerPeerState): void { state.activeSinceMs = now(); state.heartbeatMisses = 0; state.lastFailureDetail = null; this.setPhase(state, "active", null); if (state.connection?.kind === "webrtc") this.heartbeat(state, true, true); }
  private heartbeat(state: WorkerPeerState, immediate: boolean, requireFresh = false): void {
    const connection = state.connection;
    if (state.phase !== "active" || connection?.kind !== "webrtc" || activeViewCount(state) === 0 || pageHidden()) return;
    if (!immediate) {
      if (state.heartbeatTimer !== null) return;
      state.heartbeatTimer = setTimeout(
        () => { state.heartbeatTimer = null; this.heartbeat(state, true); },
        TERMINAL_PEER_HEARTBEAT_INTERVAL_MS,
      );
      return;
    }
    const heartbeatEpisode = beginTerminalPeerHeartbeatEpisode(
      state,
      connection instanceof TerminalPeerConnection ? connection : null,
      requireFresh,
    );
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
    inactive.connection?.close("terminal peer document capacity evicted inactive worker");
    return true;
  }
  private armInactiveClose(state: WorkerPeerState): void {
    if (state.connection?.kind !== "webrtc" || state.inactiveTimer !== null) return;
    state.inactiveTimer = setTimeout(() => {
      state.inactiveTimer = null;
      if (activeViewCount(state) === 0) state.connection?.close("terminal peer became inactive");
    }, PEER_INACTIVE_CLOSE_MS);
  }
  private current(state: WorkerPeerState, connection: TerminalDirectConnection, controller: AbortController, authGeneration: number): boolean {
    return state.connection === connection && state.controller === controller && !controller.signal.aborted && rootStore.auth_generation === authGeneration && activeViewCount(state) > 0;
  }
  private setPhase(state: WorkerPeerState, phase: PeerPhase, fallbackReason: FallbackReason): void {
    if (state.phase === phase && state.fallbackReason === fallbackReason) return;
    state.phase = phase; state.fallbackReason = fallbackReason;
    diag("terminal_peer.phase", { worker_fp: state.workerFp, phase, fallback_reason: fallbackReason });
  }
  private disposeState(state: WorkerPeerState, reason: string): void {
    state.controller?.abort(); state.fallbackClaims.dispose();
    if (state.retryTimer !== null) clearTimeout(state.retryTimer);
    if (state.inactiveTimer !== null) clearTimeout(state.inactiveTimer);
    if (state.heartbeatTimer !== null) clearTimeout(state.heartbeatTimer);
    state.unsubscribeGrant(); this.cancelPromotions(state, reason);
    if (state.connection?.kind === "webrtc") state.connection.close(reason);
  }
  private readonly handleVisibilityChange = (): void => {
    for (const state of this.states.values()) {
      if (pageHidden()) this.armInactiveClose(state);
      else { this.heartbeat(state, true, true); void this.maybeStart(state); }
    }
  };
  private readonly handlePageHide = (): void => {
    for (const state of this.states.values()) if (state.connection?.kind === "webrtc") state.connection.close("terminal peer page hidden");
  };
  private readonly handlePageShow = (): void => { for (const state of this.states.values()) void this.maybeStart(state); };
}

const documentTerminalPeerOwner = new TerminalPeerOwner();
export function startTerminalPeerFastPath(): void { documentTerminalPeerOwner.start(); }
export function resetTerminalPeerState(reason: string): void { documentTerminalPeerOwner.dispose(reason); }
export function stageTerminalDirectConnection(connection: TerminalDirectConnection): void { documentTerminalPeerOwner.stage(connection); }
export function retireTerminalDirectConnection(connection: TerminalDirectConnection, reason: string): void { documentTerminalPeerOwner.retire(connection, reason); }
export function terminalPeerAttemptSnapshot(workerFp: string): TerminalPeerAttemptSnapshot | null { return documentTerminalPeerOwner.snapshot(workerFp); }

