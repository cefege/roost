import { randomUUID } from "node:crypto";
import {
  TerminalStreamFailureKind,
  TerminalStreamStatus,
  type WTerminalStreamResult,
} from "@roost/shared/proto/worker_transport_pb";
import { TerminalViewStatus } from "@roost/shared/proto/sync_pb";
import { signal } from "@roost/shared/diag";
import type { TerminalGeometry } from "@roost/shared/viewport";
import { TerminalScreenHub } from "./terminal-screen-hub.ts";
import type { TerminalWorkerRequest } from "./worker-send.ts";
import { truncateTerminalReason } from "./terminal-view-protocol.ts";

export interface TerminalStreamDesired {
  streamId: string;
  enabled: boolean;
  cols: number;
  rows: number;
  retry: number;
}

export interface TerminalStreamRoute {
  workerFp: string;
  channel: number;
}

export type TerminalUnavailablePolicy = "heartbeat" | "route" | "never";

export interface TerminalStreamState {
  effective: TerminalGeometry | null;
  streamId: string;
  unavailable: boolean;
  unavailableReason: string;
  unavailablePolicy: TerminalUnavailablePolicy;
}

interface TerminalStreamSession extends TerminalStreamState {
  inFlight: TerminalStreamDesired | null;
  latest: TerminalStreamDesired | null;
}

export interface TerminalViewStreamControllerOptions {
  resolveRoute(sessionId: string): Promise<TerminalStreamRoute | null>;
  sendStream(
    workerFp: string,
    state: Omit<TerminalStreamDesired, "retry"> & { sessionId: string },
  ): TerminalWorkerRequest<WTerminalStreamResult>;
  sendSnapshot(workerFp: string, sessionId: string, streamId: string): boolean;
  geometries(sessionId: string): readonly TerminalGeometry[];
  broadcast(sessionId: string, status: TerminalViewStatus, message: string): void;
  closeViews(sessionId: string): void;
  presence(sessionId: string): void | Promise<void>;
}

export class TerminalViewStreamController {
  readonly screen: TerminalScreenHub;
  private readonly sessions = new Map<string, TerminalStreamSession>();

  constructor(private readonly options: TerminalViewStreamControllerOptions) {
    this.screen = new TerminalScreenHub({
      requestSnapshot: (sessionId, streamId) => {
        void this.requestFull(sessionId, streamId);
      },
      unavailable: (sessionId, message) => this.unavailable(sessionId, message),
    });
  }

  dispose(): void {
    this.sessions.clear();
  }

  state(sessionId: string): TerminalStreamState | null {
    return this.sessions.get(sessionId) ?? null;
  }

  recompute(sessionId: string): boolean {
    const session = this.session(sessionId);
    const geometries = this.options.geometries(sessionId);
    let effective: TerminalGeometry | null = null;
    if (geometries.length > 0) {
      let cols = geometries[0]!.cols;
      let rows = geometries[0]!.rows;
      for (let index = 1; index < geometries.length; index += 1) {
        cols = Math.min(cols, geometries[index]!.cols);
        rows = Math.min(rows, geometries[index]!.rows);
      }
      effective = { cols, rows };
    }
    if (
      effective?.cols === session.effective?.cols
      && effective?.rows === session.effective?.rows
    ) {
      void this.options.presence(sessionId);
      return false;
    }
    session.effective = effective;
    if (
      effective
      && session.unavailable
      && session.unavailablePolicy !== "heartbeat"
    ) {
      this.announceDeferred(sessionId, effective, session);
      void this.options.presence(sessionId);
      return true;
    }
    this.desire(sessionId, effective, 0);
    void this.options.presence(sessionId);
    return true;
  }

  redrive(sessionId: string): void {
    const session = this.sessions.get(sessionId);
    if (session?.effective) this.desire(sessionId, session.effective, 0);
  }

  closeSession(sessionId: string): void {
    const session = this.sessions.get(sessionId);
    if (session) {
      session.effective = null;
      session.latest = null;
      session.streamId = "";
    }
    this.options.closeViews(sessionId);
    this.sessions.delete(sessionId);
    this.screen.dropSession(sessionId);
    void this.options.presence(sessionId);
  }

  workerReplacement(workerFp: string): void {
    this.reconcileRoutes(workerFp, this.sessions.keys());
  }

  routeReconciled(workerFp: string, sessionIds: Iterable<string>): void {
    this.reconcileRoutes(workerFp, sessionIds);
  }

  private reconcileRoutes(workerFp: string, sessionIds: Iterable<string>): void {
    for (const sessionId of sessionIds) {
      const session = this.sessions.get(sessionId);
      if (
        !session?.effective
        || (session.unavailable && session.unavailablePolicy === "never")
      ) continue;
      void this.options.resolveRoute(sessionId).then((route) => {
        if (
          route?.workerFp === workerFp
          && this.sessions.get(sessionId) === session
          && session.effective
        ) {
          this.desire(sessionId, session.effective, 0);
        }
      });
    }
  }

  private session(sessionId: string): TerminalStreamSession {
    let session = this.sessions.get(sessionId);
    if (!session) {
      session = {
        effective: null,
        streamId: "",
        unavailable: false,
        unavailableReason: "",
        unavailablePolicy: "heartbeat",
        inFlight: null,
        latest: null,
      };
      this.sessions.set(sessionId, session);
    }
    return session;
  }

  private announceDeferred(
    sessionId: string,
    geometry: TerminalGeometry,
    session: TerminalStreamSession,
  ): void {
    const unavailableReason = session.unavailableReason;
    const unavailablePolicy = session.unavailablePolicy;
    session.streamId = randomUUID();
    this.screen.expectStream(sessionId, session.streamId, geometry.cols, geometry.rows);
    this.options.broadcast(sessionId, TerminalViewStatus.ACCEPTED, "");
    session.unavailable = true;
    session.unavailableReason = unavailableReason;
    session.unavailablePolicy = unavailablePolicy;
    this.options.broadcast(sessionId, TerminalViewStatus.UNAVAILABLE, unavailableReason);
  }

  private desire(sessionId: string, geometry: TerminalGeometry | null, retry: number): void {
    const session = this.session(sessionId);
    const desired: TerminalStreamDesired = {
      streamId: randomUUID(),
      enabled: geometry !== null,
      cols: geometry?.cols ?? 0,
      rows: geometry?.rows ?? 0,
      retry,
    };
    session.streamId = desired.streamId;
    session.unavailable = false;
    session.unavailableReason = "";
    session.unavailablePolicy = "heartbeat";
    session.latest = desired;
    if (geometry) {
      this.screen.expectStream(sessionId, desired.streamId, desired.cols, desired.rows);
    } else {
      this.screen.dropSession(sessionId);
    }
    this.options.broadcast(sessionId, TerminalViewStatus.ACCEPTED, "");
    void this.drive(sessionId, session);
  }

  private async drive(sessionId: string, session: TerminalStreamSession): Promise<void> {
    if (session.inFlight || !session.latest) return;
    const desired = session.latest;
    session.latest = null;
    session.inFlight = desired;
    try {
      const route = await this.options.resolveRoute(sessionId);
      if (this.sessions.get(sessionId) !== session || session.latest) return;
      if (!route) return this.unavailable(sessionId, "terminal worker is unavailable");
      const request = this.options.sendStream(route.workerFp, { sessionId, ...desired });
      if (!request.admitted) {
        void request.result.catch(() => undefined);
        return this.unavailable(sessionId, "terminal worker transport is unavailable");
      }
      try {
        this.classify(sessionId, session, desired, await request.result);
      } catch (error) {
        if (session.streamId === desired.streamId) {
          this.unavailable(
            sessionId,
            error instanceof Error ? error.message : "terminal stream result unavailable",
          );
        }
      }
    } finally {
      if (session.inFlight === desired) session.inFlight = null;
      if (session.latest) void this.drive(sessionId, session);
    }
  }

  private classify(
    sessionId: string,
    session: TerminalStreamSession,
    desired: TerminalStreamDesired,
    result: WTerminalStreamResult,
  ): void {
    if (
      result.sessionId !== sessionId
      || result.streamId !== desired.streamId
      || result.enabled !== desired.enabled
    ) {
      signal("terminal.stream_result_mismatch", { session_id: sessionId });
      if (session.streamId === desired.streamId) {
        this.screen.failClosed(sessionId, "terminal worker returned a mismatched stream result");
        this.unavailable(
          sessionId,
          "terminal worker returned a mismatched stream result",
          "never",
        );
      }
      return;
    }
    if (session.streamId !== desired.streamId) return;
    if (result.status === TerminalStreamStatus.COMMITTED) {
      if (
        desired.enabled
        && (result.effectiveCols !== desired.cols || result.effectiveRows !== desired.rows)
      ) {
        signal("terminal.stream_invariant_failure", {
          session_id: sessionId,
          failure_kind: "committed_geometry_mismatch",
        });
        this.screen.failClosed(sessionId, "terminal worker committed unexpected geometry");
        this.unavailable(sessionId, "terminal worker committed unexpected geometry", "never");
      }
      return;
    }
    if (
      result.failureKind === TerminalStreamFailureKind.RETRYABLE_PRE_WRITE
      && desired.retry === 0
    ) {
      this.desire(sessionId, session.effective, 1);
      return;
    }

    const message = result.reason || "terminal stream is unavailable";
    switch (result.failureKind) {
      case TerminalStreamFailureKind.RETRYABLE_PRE_WRITE:
        this.unavailable(sessionId, message, "heartbeat");
        return;
      case TerminalStreamFailureKind.SESSION_NOT_LIVE:
      case TerminalStreamFailureKind.AMBIGUOUS_BOUNDARY:
        this.screen.failClosed(sessionId, message);
        this.unavailable(sessionId, message, "route");
        return;
      case TerminalStreamFailureKind.CORE_FAILED:
        signal("terminal.stream_invariant_failure", {
          session_id: sessionId,
          failure_kind: result.failureKind,
        });
        this.screen.failClosed(sessionId, message);
        this.unavailable(sessionId, message, "route");
        return;
      case TerminalStreamFailureKind.INVALID_REQUEST:
        signal("terminal.stream_invariant_failure", {
          session_id: sessionId,
          failure_kind: result.failureKind,
        });
        this.screen.failClosed(sessionId, message);
        this.unavailable(sessionId, message, "never");
        return;
      case TerminalStreamFailureKind.UNSPECIFIED:
      default:
        this.screen.failClosed(sessionId, message);
        this.unavailable(sessionId, message, "never");
    }
  }

  private unavailable(
    sessionId: string,
    message: string,
    policy: TerminalUnavailablePolicy = "heartbeat",
  ): void {
    const session = this.sessions.get(sessionId);
    if (!session?.effective) return;
    session.unavailable = true;
    session.unavailableReason = truncateTerminalReason(message);
    session.unavailablePolicy = policy;
    this.options.broadcast(sessionId, TerminalViewStatus.UNAVAILABLE, session.unavailableReason);
  }

  private async requestFull(sessionId: string, streamId: string): Promise<void> {
    const session = this.sessions.get(sessionId);
    if (!session?.effective || session.streamId !== streamId) return;
    const route = await this.options.resolveRoute(sessionId);
    if (!route || this.sessions.get(sessionId)?.streamId !== streamId) return;
    if (!this.options.sendSnapshot(route.workerFp, sessionId, streamId)) {
      this.unavailable(sessionId, "snapshot request could not reach worker");
    }
  }
}
