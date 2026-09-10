// Desired-state reconciler for per-session terminal streams toward workers:
// computes the effective geometry (min across viewers) and drives it to the
// routed worker, coalescing concurrent requests via inFlight/latest. Each
// desire mints a fresh streamId and must call screen.expectStream BEFORE
// broadcasting ACCEPTED, or snapshot requests race the worker's first frame.
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
import {
  TERMINAL_STREAM_CONTROL_TIMEOUT_MS,
  startHopDeadline,
  type HopDeadline,
} from "./worker-send.ts";
import { truncateTerminalReason } from "./terminal-view-protocol.ts";
import type {
  TerminalStreamDesired,
  TerminalStreamState,
  TerminalUnavailablePolicy,
  TerminalViewStreamControllerOptions,
} from "./terminal-view-stream-controller-types.ts";

export type {
  TerminalStreamDesired,
  TerminalStreamRoute,
  TerminalStreamState,
  TerminalUnavailablePolicy,
  TerminalViewStreamControllerOptions,
} from "./terminal-view-stream-controller-types.ts";

type TerminalStreamWork = TerminalStreamDesired & { deadline: HopDeadline };
interface TerminalStreamSession extends TerminalStreamState {
  inFlight: TerminalStreamWork | null;
  latest: TerminalStreamWork | null;
  replacementWork: TerminalStreamWork | null;
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
      requestFreshStream: (sessionId, expectedStreamId, reason) => {
        this.redriveFreshStream(sessionId, expectedStreamId, reason);
      },
    });
  }
  dispose(): void {
    this.options.streamDispatcher.dispose();
    this.screen.dispose();
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
    if (session?.effective && session.unavailablePolicy !== "route") {
      this.desire(sessionId, session.effective, 0);
    }
  }
  closeSession(sessionId: string): void {
    this.options.streamDispatcher.cancelSession(sessionId);
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
    this.options.streamDispatcher.workerReplacement(workerFp);
    this.reconcileRoutes(workerFp, this.sessions.keys());
  }
  routeReconciled(workerFp: string, sessionIds: Iterable<string>): void {
    this.reconcileRoutes(workerFp, sessionIds);
  }
  workerRetired(workerFp: string, sessionIds: Iterable<string>): void {
    const retiredSessionIds = [...sessionIds];
    this.options.streamDispatcher.workerRetired(workerFp, retiredSessionIds);
    for (const sessionId of retiredSessionIds) this.closeSession(sessionId);
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
          const currentWork = session.replacementWork?.streamId === session.streamId
            ? session.replacementWork
            : session.latest?.streamId === session.streamId
              ? session.latest
              : session.inFlight?.streamId === session.streamId ? session.inFlight : null;
          this.desire(sessionId, session.effective, 0, currentWork?.deadline);
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
        replacementWork: null,
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
  private desire(sessionId: string, geometry: TerminalGeometry | null, retry: number, deadline?: HopDeadline): void {
    const session = this.session(sessionId);
    session.replacementWork = null;
    const desired: TerminalStreamWork = {
      streamId: randomUUID(),
      enabled: geometry !== null,
      cols: geometry?.cols ?? 0,
      rows: geometry?.rows ?? 0,
      retry,
      deadline: deadline ?? this.options.createStreamDeadline?.() ?? startHopDeadline(TERMINAL_STREAM_CONTROL_TIMEOUT_MS),
    };
    session.streamId = desired.streamId;
    session.unavailable = false;
    session.unavailableReason = "";
    session.unavailablePolicy = "heartbeat";
    session.latest = desired;
    this.options.streamDispatcher.cancelSession(sessionId, "superseded");
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
      const dispatch = this.options.streamDispatcher.enqueue({
        sessionId,
        streamId: desired.streamId,
        enabled: desired.enabled,
        cols: desired.cols,
        rows: desired.rows,
        deadline: desired.deadline,
      });
      const completion = await dispatch.completion;
      if (completion.kind === "cancelled") {
        if (session.streamId === desired.streamId) {
          if (completion.reason === "route_changed") {
            this.desire(sessionId, session.effective, 0, desired.deadline);
          } else if (completion.reason === "worker_generation_replaced") {
            session.replacementWork = desired;
            this.unavailable(sessionId, "terminal worker is unavailable", "route");
          }
        }
        return;
      }
      const request = completion.request;
      if (!request.admitted) {
        void request.result.catch(() => undefined);
        if (session.streamId === desired.streamId) {
          this.unavailable(sessionId, "terminal worker transport is unavailable");
        }
        return;
      }
      try {
        this.classify(sessionId, session, desired, await request.result);
      } catch (error) {
        if (session.streamId === desired.streamId && completion.workerGenerationReplaced?.()) {
          session.replacementWork = desired;
          this.unavailable(sessionId, "terminal worker is unavailable", "route");
        } else if (session.streamId === desired.streamId) {
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

  private redriveFreshStream(sessionId: string, expectedStreamId: string, _reason: string): void {
    const session = this.sessions.get(sessionId);
    if (
      !session?.effective || session.streamId !== expectedStreamId
      || session.unavailablePolicy === "route"
    ) return;
    this.desire(sessionId, session.effective, 0);
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
