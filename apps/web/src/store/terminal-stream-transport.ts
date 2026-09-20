// One document-owned direct terminal registry. WebSocket and RTC adapters only
// register authenticated connections here; publication and retargeting choose
// the elected per-session route without importing transport implementation.
// A candidate has no canonical effect until its synchronous promotion commits.

import type { CellGridFrame } from "@roost/shared/cell";
import type { LocalScrollbackResponse } from "@roost/shared/proto/local_terminal_pb";
import type {
  InputCommand,
  TerminalInputRouteClaim,
  TerminalInputRouteResult,
  TerminalResyncCommand,
  TerminalViewCommand,
} from "@roost/shared/proto/sync_pb";
import {
  terminalGenerationTokenEquals,
  type TerminalGenerationToken,
  type TerminalViewIntent,
} from "./terminal-stream-types.ts";

export interface LocalScrollbackQuery {
  sessionId: string;
  endRow: bigint;
  maxRows: number;
  gridEpoch: string;
}

export type TerminalPeerCandidateType = "host" | "srflx" | "prflx" | "none";

export interface TerminalDirectConnectionTelemetry {
  readonly opaquePeerId: string | null;
  readonly lastProbeAtMs: number | null;
  readonly rttMs: number | null;
  /** Current content-free worker probe proof; false after foreground suspension/stall. */
  readonly livenessQualified?: boolean;
  /** Worker-side selected ICE candidate class; no endpoint metadata is retained. */
  readonly candidateType?: TerminalPeerCandidateType;
  readonly bufferedBytes: number | null;
}


export interface TerminalDirectConnection {
  readonly workerFp: string;
  readonly kind: "loopback" | "webrtc";
  readonly connectionId: string;
  readonly workerEpoch: string;
  readonly inputRouteSupported: boolean;
  token(): TerminalGenerationToken | null;
  allowsSession(sessionId: string): boolean;
  publishView(command: TerminalViewCommand): boolean;
  publishResync(command: TerminalResyncCommand): boolean;
  sendInput(command: InputCommand): "accepted" | "refused";
  claimInputRoute(command: TerminalInputRouteClaim): Promise<TerminalInputRouteResult>;
  requestScrollback(query: LocalScrollbackQuery): Promise<LocalScrollbackResponse>;
  probe(requestId: string): Promise<void>;
  close(reason: string): void;
  telemetry?(): TerminalDirectConnectionTelemetry;
}

export interface TerminalDirectPromotionPreparedView {
  readonly viewId: string;
  readonly intent: TerminalViewIntent;
  readonly acknowledged: boolean;
}

export interface TerminalDirectPromotionPrepared {
  readonly attemptId: string;
  readonly connection: TerminalDirectConnection;
  readonly token: TerminalGenerationToken;
  readonly oldToken: TerminalGenerationToken | null;
  readonly currentToken: TerminalGenerationToken | null;
  readonly claimEpoch: string;
  readonly candidateFrame: CellGridFrame;
  readonly expectedStreamId: string;
  readonly prospectiveViews: ReadonlyMap<string, TerminalDirectPromotionPreparedView>;
  /** Atomically installs the already-validated candidate state without notifying. */
  applyCanonical(): boolean;
}

export type TerminalDirectRegistryEvent =
  | {
      kind: "demand_changed";
      workerFp: string;
      sessionId: string;
      viewId: string;
      active: boolean;
    }
  | {
      kind: "promotion_committed";
      sessionId: string;
      token: TerminalGenerationToken;
    }
  | {
      kind: "route_lost";
      sessionId: string;
      token: TerminalGenerationToken;
      reason: string;
    }
  | {
      kind: "worker_retired";
      workerFp: string;
      reason: string;
    };

interface TerminalDirectWorkerConnections {
  active: TerminalDirectConnection | null;
  candidate: TerminalDirectConnection | null;
}

interface TerminalDirectRoute {
  connection: TerminalDirectConnection;
  token: TerminalGenerationToken;
}

/**
 * Elects direct routes only after a candidate baseline has been validated.
 * It owns bounded per-worker slots and all per-session route identity.
 */
export class TerminalDirectRegistry {
  readonly #connections = new Map<string, TerminalDirectWorkerConnections>();
  readonly #routes = new Map<string, TerminalDirectRoute>();
  readonly #demands = new Map<string, Map<string, Set<string>>>();
  readonly #listeners = new Set<(event: TerminalDirectRegistryEvent) => void>();
  #resetting = false;

  register(connection: TerminalDirectConnection): () => void {
    if (this.#resetting) {
      connection.close("terminal direct registry reset");
      return () => undefined;
    }
    const token = connection.token();
    if (token && !isConnectionToken(connection, token)) {
      connection.close("invalid terminal direct generation");
      return () => undefined;
    }
    let workerConnections = this.#connections.get(connection.workerFp);
    if (!workerConnections) {
      workerConnections = { active: null, candidate: connection };
      this.#connections.set(connection.workerFp, workerConnections);
      return () => this.#unregister(connection, "connection closed");
    }
    if (
      workerConnections.active === connection
      || workerConnections.candidate === connection
    ) {
      return () => this.#unregister(connection, "connection closed");
    }
    const displaced = workerConnections.candidate;
    if (displaced && this.#hasRouteForConnection(displaced)) {
      connection.close("terminal direct candidate capacity reached");
      return () => undefined;
    }
    workerConnections.candidate = connection;
    displaced?.close("terminal direct candidate replaced");
    return () => this.#unregister(connection, "connection closed");
  }

  candidateForWorker(workerFp: string): TerminalDirectConnection | null {
    return this.#connections.get(workerFp)?.candidate ?? null;
  }

  activeForSession(sessionId: string): TerminalDirectConnection | null {
    const route = this.#routes.get(sessionId);
    if (
      !route
      || !route.connection.allowsSession(sessionId)
      || !terminalGenerationTokenEquals(route.connection.token(), route.token)
    ) return null;
    return route.connection;
  }

  targetForToken(token: TerminalGenerationToken): TerminalDirectConnection | null {
    if (token.transportKind === "sync" || !token.workerFp) return null;
    const connections = this.#connections.get(token.workerFp);
    for (const connection of [connections?.active, connections?.candidate]) {
      if (
        connection
        && isConnectionToken(connection, token)
        && terminalGenerationTokenEquals(connection.token(), token)
      ) return connection;
    }
    return null;
  }

  hasViewDemand(workerFp: string, sessionId: string): boolean {
    return (this.#demands.get(workerFp)?.get(sessionId)?.size ?? 0) > 0;
  }

  setViewDemand(
    workerFp: string,
    sessionId: string,
    viewId: string,
    active: boolean,
  ): void {
    if (this.#resetting) return;
    const sessions = this.#demands.get(workerFp);
    const views = sessions?.get(sessionId);
    if (active) {
      if (views?.has(viewId)) return;
      const nextSessions = sessions ?? new Map<string, Set<string>>();
      const nextViews = views ?? new Set<string>();
      nextViews.add(viewId);
      nextSessions.set(sessionId, nextViews);
      this.#demands.set(workerFp, nextSessions);
    } else {
      if (!sessions || !views?.delete(viewId)) return;
      if (views.size === 0) sessions.delete(sessionId);
      if (sessions.size === 0) this.#demands.delete(workerFp);
    }
    this.#emit({ kind: "demand_changed", workerFp, sessionId, viewId, active });
  }

  subscribe(listener: (event: TerminalDirectRegistryEvent) => void): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  reset(reason: string): void {
    if (this.#resetting) return;
    this.#resetting = true;
    try {
      const routes = [...this.#routes.entries()];
      const connections = new Set<TerminalDirectConnection>();
      for (const workerConnections of this.#connections.values()) {
        if (workerConnections.active) connections.add(workerConnections.active);
        if (workerConnections.candidate) connections.add(workerConnections.candidate);
      }
      this.#routes.clear();
      this.#connections.clear();
      this.#demands.clear();
      for (const connection of connections) connection.close(reason);
      for (const [sessionId, route] of routes) {
        this.#emit({ kind: "route_lost", sessionId, token: route.token, reason });
      }
    } finally {
      this.#resetting = false;
    }
  }

  retireWorker(workerFp: string, reason: string): void {
    if (this.#resetting) return;
    const workerConnections = this.#connections.get(workerFp);
    const connections = new Set([
      workerConnections?.active,
      workerConnections?.candidate,
    ].filter((connection): connection is TerminalDirectConnection => connection !== null && connection !== undefined));
    const lostRoutes: Array<[string, TerminalDirectRoute]> = [];
    for (const [sessionId, route] of this.#routes) {
      if (route.connection.workerFp !== workerFp) continue;
      this.#routes.delete(sessionId);
      lostRoutes.push([sessionId, route]);
    }
    this.#connections.delete(workerFp);
    this.#demands.delete(workerFp);
    for (const connection of connections) connection.close(reason);
    for (const [sessionId, route] of lostRoutes) {
      this.#emit({ kind: "route_lost", sessionId, token: route.token, reason });
    }
    this.#emit({ kind: "worker_retired", workerFp, reason });
  }

  commitSessionPromotion(
    sessionId: string,
    attemptId: string,
    prepared: TerminalDirectPromotionPrepared,
  ): boolean {
    const connection = prepared.connection;
    const workerConnections = this.#connections.get(connection.workerFp);
    const previousRoute = this.#routes.get(sessionId);
    if (
      this.#resetting
      || prepared.attemptId !== attemptId
      || !workerConnections
      || (workerConnections.active !== connection && workerConnections.candidate !== connection)
      || (this.#demands.get(connection.workerFp)?.get(sessionId)?.size ?? 0) === 0
      || !isConnectionToken(connection, prepared.token)
      || !terminalGenerationTokenEquals(connection.token(), prepared.token)
      || !terminalGenerationTokenEquals(prepared.currentToken, prepared.oldToken)
      || !connection.allowsSession(sessionId)
      || (
        workerConnections.active !== null
        && workerConnections.active !== connection
        && connection.kind !== "loopback"
      )
    ) return false;

    // Candidate folding owns its own state, but it cannot publish it until all
    // route checks above pass. No listener runs until every owner is current.
    if (!prepared.applyCanonical()) return false;
    this.#migrateDemandViews(
      connection.workerFp,
      sessionId,
      prepared.prospectiveViews,
    );
    this.#routes.set(sessionId, { connection, token: prepared.token });
    this.#promoteCandidateIfPossible(connection.workerFp);
    this.#emit({ kind: "promotion_committed", sessionId, token: prepared.token });
    return true;
  }

  retireSessionRoute(
    sessionId: string,
    expectedToken: TerminalGenerationToken,
    reason: string,
  ): void {
    const route = this.#routes.get(sessionId);
    if (!route || !terminalGenerationTokenEquals(route.token, expectedToken)) return;
    this.#routes.delete(sessionId);
    this.#promoteCandidateIfPossible(route.connection.workerFp);
    this.#emit({ kind: "route_lost", sessionId, token: route.token, reason });
  }

  #unregister(connection: TerminalDirectConnection, reason: string): void {
    const workerConnections = this.#connections.get(connection.workerFp);
    const wasActive = workerConnections?.active === connection;
    const wasCandidate = workerConnections?.candidate === connection;
    if (!wasActive && !wasCandidate) return;
    if (wasActive) workerConnections!.active = null;
    if (wasCandidate) workerConnections!.candidate = null;

    const lostRoutes: Array<[string, TerminalDirectRoute]> = [];
    for (const [sessionId, route] of this.#routes) {
      if (route.connection !== connection) continue;
      this.#routes.delete(sessionId);
      lostRoutes.push([sessionId, route]);
    }
    this.#promoteCandidateIfPossible(connection.workerFp);
    const currentConnections = this.#connections.get(connection.workerFp);
    if (
      currentConnections
      && !currentConnections.active
      && !currentConnections.candidate
    ) this.#connections.delete(connection.workerFp);
    for (const [sessionId, route] of lostRoutes) {
      this.#emit({ kind: "route_lost", sessionId, token: route.token, reason });
    }
  }

  #migrateDemandViews(
    workerFp: string,
    sessionId: string,
    prospectiveViews: ReadonlyMap<string, TerminalDirectPromotionPreparedView>,
  ): void {
    const demandedViews = this.#demands.get(workerFp)?.get(sessionId);
    if (!demandedViews) return;
    for (const [oldViewId, prospective] of prospectiveViews) {
      if (!demandedViews.delete(oldViewId)) continue;
      demandedViews.add(prospective.viewId);
    }
  }

  #hasRouteForConnection(connection: TerminalDirectConnection): boolean {
    for (const route of this.#routes.values()) {
      if (route.connection === connection) return true;
    }
    return false;
  }

  #promoteCandidateIfPossible(workerFp: string): void {
    const workerConnections = this.#connections.get(workerFp);
    if (
      !workerConnections?.candidate
      || !this.#hasRouteForConnection(workerConnections.candidate)
    ) return;
    if (
      workerConnections.active !== null
      && this.#hasRouteForConnection(workerConnections.active)
    ) return;
    workerConnections.active = workerConnections.candidate;
    workerConnections.candidate = null;
  }

  #emit(event: TerminalDirectRegistryEvent): void {
    for (const listener of [...this.#listeners]) listener(event);
  }
}

function isConnectionToken(
  connection: TerminalDirectConnection,
  token: TerminalGenerationToken,
): boolean {
  return Number.isSafeInteger(token.socketGeneration)
    && token.socketGeneration >= 0
    && token.transportKind === connection.kind
    && token.workerFp === connection.workerFp
    && token.processEpoch === connection.workerEpoch;
}

/** The sole production owner. Tests reset this instance between cases. */
export const terminalDirectRegistry = new TerminalDirectRegistry();
