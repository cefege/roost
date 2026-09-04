// Coord-side owner of socket-bound terminal view membership: which viewer
// sockets watch which session, the route reconciliation when a worker
// reconnects, and the projection snapshot Sync seeds. All registry mutation
// flows through TerminalViewHub methods — the module-level productionHub is
// install-once; reading it from outside (activeTerminalViewerFingerprints)
// must tolerate "no hub installed" because tests construct hubs directly.
import type { WTerminalStreamResult } from "@roost/shared/proto/worker_transport_pb";
import type {
  TerminalResyncCommand,
  TerminalViewCommand,
} from "@roost/shared/proto/sync_pb";
import {
  TERMINAL_VIEW_SWEEP_MS,
  type TerminalGeometry,
} from "@roost/shared/viewport";
import { globalPresenceBus, sessionBus } from "../buses.ts";
import type { KyselyDB } from "../db/connection.ts";
import { resolveSessionRoute } from "./terminal-control-lane.ts";
import {
  sendTerminalSnapshotRequest,
  sendTerminalStreamStateRequest,
  type TerminalWorkerRequest,
} from "./worker-send.ts";
import {
  TerminalScreenHub,
  type TerminalScreenSocketSink,
} from "./terminal-screen-hub.ts";
import { TerminalViewRegistry } from "./terminal-view-registry.ts";
import {
  TerminalViewStreamController,
  type TerminalStreamDesired,
  type TerminalStreamRoute,
} from "./terminal-view-stream-controller.ts";

export interface TerminalViewHubOptions {
  db: KyselyDB;
  now?: () => number;
  resolveRoute?: (
    dashboardId: string,
    sessionId: string,
  ) => Promise<TerminalStreamRoute | null>;
  sendStreamState?: (
    workerFp: string,
    state: Omit<TerminalStreamDesired, "retry"> & {
      sessionId: string;
      dashboardId: string;
    },
  ) => TerminalWorkerRequest<WTerminalStreamResult>;
  sendSnapshot?: (
    workerFp: string,
    sessionId: string,
    streamId: string,
    dashboardId: string,
  ) => boolean;
}

export interface TerminalSocketRegistration {
  socketId: string;
  viewerKey: string | null;
  callerFingerprint: string;
  /** Server-selected dashboard, not a terminal command field. */
  dashboardId: string;
  allowsSession(sessionId: string): boolean;
  sink: TerminalScreenSocketSink;
}

let productionHub: TerminalViewHub | null = null;

export function installTerminalViewHub(hub: TerminalViewHub | null): void {
  productionHub = hub;
}

export function activeTerminalViewerFingerprints(sessionId: string): ReadonlySet<string> {
  return productionHub?.activeViewerFingerprints(sessionId) ?? new Set();
}

export function terminalViewerProjection(): ReadonlyMap<string, ReadonlyMap<string, TerminalGeometry>> {
  return productionHub?.viewerProjection() ?? new Map();
}


export function currentTerminalScreenHub(): TerminalScreenHub | null {
  return productionHub?.screen ?? null;
}

export function terminalViewSnapshot(
  sessionId: string,
): ReturnType<TerminalViewHub["snapshot"]> {
  return productionHub?.snapshot(sessionId) ?? null;
}

export function notifyTerminalRouteReconciled(
  workerFp: string,
  sessionIds: Iterable<string>,
): void {
  productionHub?.routeReconciled(workerFp, sessionIds);
}

export function notifyTerminalWorkerRetired(
  workerFp: string,
  sessionIds: Iterable<string>,
): void {
  productionHub?.workerRetired(workerFp, sessionIds);
}

/**
 * Coordinator owner for socket-bound terminal view membership and the one
 * effective worker stream per watched session. Membership and stream lifecycle
 * are split into single-owner collaborators; this facade preserves the public
 * API and wires their mutually dependent notifications.
 */
export class TerminalViewHub {
  readonly screen: TerminalScreenHub;
  private readonly now: () => number;
  private readonly registry: TerminalViewRegistry;
  private readonly streams: TerminalViewStreamController;
  private readonly timer: ReturnType<typeof setInterval>;
  private readonly unsubscribe: () => void;
  private onLiveViewExpired:
    (socketId: string, viewId: string, sessionId: string) => void = () => undefined;

  constructor(options: TerminalViewHubOptions) {
    this.now = options.now ?? Date.now;
    const resolveRoute = options.resolveRoute
      ?? ((dashboardId: string, sessionId: string) =>
        resolveSessionRoute(options.db, dashboardId, sessionId));
    const sendStream = options.sendStreamState
      ?? ((workerFp: string, state: Omit<TerminalStreamDesired, "retry"> & {
        sessionId: string;
        dashboardId: string;
      }) => sendTerminalStreamStateRequest(workerFp, state));
    const sendSnapshot = options.sendSnapshot
      ?? ((workerFp: string, sessionId: string, streamId: string, dashboardId: string) =>
        sendTerminalSnapshotRequest(workerFp, { sessionId, streamId, dashboardId }));

    this.streams = new TerminalViewStreamController({
      resolveRoute,
      sendStream,
      sendSnapshot,
      geometries: (sessionId) => this.registry.geometries(sessionId),
      broadcast: (sessionId, status, message) => {
        this.registry.broadcast(sessionId, status, message);
      },
      closeViews: (sessionId) => this.registry.closeSession(sessionId),
      presence: (sessionId) => this.presence(sessionId),
    });
    this.screen = this.streams.screen;
    this.registry = new TerminalViewRegistry({
      screen: this.screen,
      now: this.now,
      streamState: (sessionId) => this.streams.state(sessionId),
      recompute: (sessionId, dashboardId) => this.streams.recompute(sessionId, dashboardId),
      redrive: (sessionId) => this.streams.redrive(sessionId),
      onLiveViewExpired: (socketId, viewId, sessionId) => {
        this.onLiveViewExpired(socketId, viewId, sessionId);
      },
    });

    this.timer = setInterval(() => this.sweep(), TERMINAL_VIEW_SWEEP_MS);
    this.timer.unref?.();
    this.unsubscribe = sessionBus.subscribe((event) => {
      if (event.kind === "closed") this.closeSession(String(event.session_id));
    });
  }

  dispose(): void {
    clearInterval(this.timer);
    this.onLiveViewExpired = () => undefined;
    this.unsubscribe();
    this.registry.dispose();
    this.streams.dispose();
  }

  registerSocket(registration: TerminalSocketRegistration): void {
    this.registry.registerSocket(registration);
  }

  closeSocket(socketId: string): void {
    this.registry.closeSocket(socketId);
  }
  setOnLiveViewExpired(
    handler: ((socketId: string, viewId: string, sessionId: string) => void) | null,
  ): void {
    this.onLiveViewExpired = handler ?? (() => undefined);
  }


  removeFingerprint(fingerprint: string): void {
    this.registry.removeFingerprint(fingerprint);
  }

  removeDashboard(dashboardId: string): void {
    this.registry.removeDashboard(dashboardId);
  }

  handleViewCommand(socketId: string, command: TerminalViewCommand): void {
    this.registry.handleViewCommand(socketId, command);
  }

  handleResync(socketId: string, command: TerminalResyncCommand): void {
    this.registry.handleResync(socketId, command);
  }

  workerReplacement(workerFp: string): void {
    this.streams.workerReplacement(workerFp);
  }

  routeReconciled(workerFp: string, sessionIds: Iterable<string>): void {
    this.streams.routeReconciled(workerFp, sessionIds);
  }

  workerRetired(workerFp: string, sessionIds: Iterable<string>): void {
    this.streams.workerRetired(workerFp, sessionIds);
  }

  closeSession(sessionId: string): void {
    this.streams.closeSession(sessionId);
  }

  activeViewerFingerprints(sessionId: string): ReadonlySet<string> {
    return this.registry.activeViewerFingerprints(sessionId);
  }

  viewerProjection(): ReadonlyMap<string, ReadonlyMap<string, TerminalGeometry>> {
    return this.registry.viewerProjection();
  }

  snapshot(sessionId: string): {
    activeViews: number;
    parkedViews: number;
    streamId: string;
    effective: TerminalGeometry | null;
    unavailable: boolean;
  } | null {
    const stream = this.streams.state(sessionId);
    if (!stream) return null;
    const views = this.registry.viewStats(sessionId);
    return {
      ...views,
      streamId: stream.streamId,
      effective: stream.effective && { ...stream.effective },
      unavailable: stream.unavailable,
    };
  }

  private sweep(): void {
    this.registry.sweep();
  }

  private async presence(sessionId: string): Promise<void> {
    const viewers = this.registry.viewerProjection().get(sessionId) ?? new Map();
    const entries = [...viewers].map(([fp, geometry]) => ({
      fp,
      cols: geometry.cols,
      rows: geometry.rows,
      lastMs: this.now(),
    }));
    globalPresenceBus.publish({
      session_id: sessionId,
      data: { kind: "viewers", fps: entries.map((entry) => entry.fp), entries },
    });
  }
}
