// Coord-side owner of socket-bound terminal view membership: which viewer
// sockets watch which session, the route reconciliation when a worker
// reconnects, and the projection snapshot Sync seeds. A session whose worker
// advertised terminal-view-owner-v1 skips all of that and is relayed instead —
// exactly one minimizer per session. All registry mutation flows through
// TerminalViewHub methods — the module-level productionHub is install-once;
// reading it from outside (activeTerminalViewerFingerprints) must tolerate
// "no hub installed" because tests construct hubs directly.
import type {
  WTerminalStreamResult,
  WTerminalViewProjection,
  WTerminalViewState,
} from "@roost/protocol/proto/worker_transport_pb";
import type {
  TerminalResyncCommand,
  TerminalViewCommand,
  TerminalViewStateFrame,
} from "@roost/protocol/proto/sync_pb";
import {
  TERMINAL_VIEW_SWEEP_MS,
  type TerminalGeometry,
} from "@roost/protocol/viewport";
import { globalPresenceBus, sessionBus } from "../../events/buses.ts";
import type { KyselyDB } from "../../db/connection.ts";
import { resolveSessionRoute } from "../input/terminal-control-lane.ts";
import {
  sendTerminalSnapshotRequest,
  sendTerminalStreamStateRequest,
  type HopDeadline,
  type TerminalWorkerRequest,
} from "../../workers/worker-send.ts";
import { TerminalStreamDispatcher } from "../screen/terminal-stream-dispatcher.ts";
import { currentRoutableWorker } from "../../workers/worker-send-target.ts";
import {
  TerminalScreenHub,
  type TerminalScreenSocketSink,
} from "../screen/terminal-screen-hub.ts";
import {
  TerminalViewRegistry,
  type TerminalViewInput,
} from "@roost/protocol/terminal-view";
import { TerminalViewStreamController } from "./terminal-view-stream-controller.ts";
import type { TerminalScreenCaps } from "../screen/terminal-screen-budget.ts";
import type {
  TerminalStreamDesired,
  TerminalStreamRoute,
} from "./terminal-view-stream-controller-types.ts";
import { TerminalViewOwnerRelay } from "./terminal-view-owner-relay.ts";
import {
  applyTerminalViewProjection,
  dropTerminalViewProjection,
  mergeOwnerTerminalViewerProjection,
  ownerTerminalViewerFingerprints,
  ownerTerminalViewerGeometry,
  terminalViewOwnerForSession,
  terminalViewOwnerRow,
} from "./terminal-view-projection.ts";
import {
  sendTerminalViewRelay,
  sendTerminalViewSocketClosed,
  type TerminalViewRelayCommand,
  type TerminalViewRelayIdentity,
} from "./worker-send-terminal-view.ts";

/** What diagnostics read about one session: live vs parked membership plus the
 * stream the coordinator currently owns for it. */
export interface TerminalViewSnapshot {
  activeViews: number;
  parkedViews: number;
  streamId: string;
  effective: TerminalGeometry | null;
  unavailable: boolean;
}

export interface TerminalViewHubOptions {
  db: KyselyDB;
  now?: () => number;
  createStreamDeadline?: () => HopDeadline;
  resolveRoute?: (sessionId: string) => Promise<TerminalStreamRoute | null>;
  currentWorker?: (workerFp: string) => unknown | null;
  sendStreamState?: (
    workerFp: string,
    state: Omit<TerminalStreamDesired, "retry"> & { sessionId: string },
    deadline?: HopDeadline,
  ) => TerminalWorkerRequest<WTerminalStreamResult>;
  sendSnapshot?: (workerFp: string, sessionId: string, streamId: string) => boolean;
  /** Owner-mode routing and relay, injected so hermetic tests need neither the
   * process-global worker registry nor the byte-hub route cache. */
  ownerForSession?: (sessionId: string) => string | null;
  sendViewRelay?: (
    workerFp: string,
    identity: TerminalViewRelayIdentity,
    command: TerminalViewRelayCommand,
  ) => boolean;
  sendViewSocketClosed?: (workerFp: string, socketId: string) => boolean;
  terminalScreen?: TerminalScreenCaps;
}

export interface TerminalSocketRegistration {
  socketId: string;
  viewerKey: string | null;
  callerFingerprint: string;
  allowsSession(sessionId: string): boolean;
  sink: TerminalScreenSocketSink;
}

let productionHub: TerminalViewHub | null = null;

export function installTerminalViewHub(hub: TerminalViewHub | null): void {
  productionHub = hub;
}

export function activeTerminalViewerFingerprints(sessionId: string): ReadonlySet<string> {
  return ownerTerminalViewerFingerprints(sessionId)
    ?? productionHub?.activeViewerFingerprints(sessionId)
    ?? new Set();
}

export function terminalViewerProjection(): ReadonlyMap<string, ReadonlyMap<string, TerminalGeometry>> {
  const merged = new Map(productionHub?.viewerProjection() ?? []);
  mergeOwnerTerminalViewerProjection(merged);
  return merged;
}

export function currentTerminalScreenHub(): TerminalScreenHub | null {
  return productionHub?.screen ?? null;
}

export function terminalViewSnapshot(sessionId: string): TerminalViewSnapshot | null {
  const owned = terminalViewOwnerRow(sessionId);
  if (!owned) return productionHub?.snapshot(sessionId) ?? null;
  let parkedViews = 0;
  for (const viewer of owned.viewers) if (viewer.parked) parkedViews += 1;
  return {
    activeViews: owned.viewers.length - parkedViews,
    parkedViews,
    streamId: owned.streamId,
    effective: owned.effective && { ...owned.effective },
    // The owning worker publishes the stream it actually holds; the
    // coordinator drives no desire for this session and so has no failure of
    // its own to report.
    unavailable: false,
  };
}

/** Per-viewer geometry inputs behind a session's effective geometry. Empty
 * when no production hub is installed (tests construct hubs directly). */
export function terminalViewInputs(sessionId: string): readonly TerminalViewInput[] {
  return terminalViewOwnerRow(sessionId)?.viewers
    ?? productionHub?.viewerInputs(sessionId)
    ?? [];
}

/** One view decision from a worker that owns its own terminal views. */
export function dispatchWorkerTerminalViewState(
  workerFp: string,
  state: WTerminalViewState,
): void {
  if (!state.frame) return;
  productionHub?.applyOwnerViewState(workerFp, state.socketId, state.frame);
}

/** The owning worker's membership for one session, replacing whatever the
 * coordinator last projected for it. */
export function applyWorkerTerminalViewProjection(
  workerFp: string,
  projection: WTerminalViewProjection,
): void {
  applyTerminalViewProjection(workerFp, projection);
  productionHub?.publishPresence(projection.sessionId);
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

/** Coordinator owner for socket-bound terminal view membership and the one
 * effective worker stream per watched session. Membership and stream lifecycle
 * are split into single-owner collaborators; this facade preserves the public
 * API and wires their mutually dependent notifications. */
export class TerminalViewHub {
  readonly screen: TerminalScreenHub;
  private readonly now: () => number;
  private readonly registry: TerminalViewRegistry;
  private readonly streams: TerminalViewStreamController;
  private readonly timer: ReturnType<typeof setInterval>;
  private readonly unsubscribe: () => void;
  private onLiveViewExpired:
    (socketId: string, viewId: string, sessionId: string) => void = () => undefined;
  private readonly registrations = new Map<string, TerminalSocketRegistration>();
  private readonly relay: TerminalViewOwnerRelay;
  private readonly ownerForSession: (sessionId: string) => string | null;

  constructor(options: TerminalViewHubOptions) {
    this.now = options.now ?? Date.now;
    const resolveRoute = options.resolveRoute
      ?? ((sessionId: string) => resolveSessionRoute(options.db, sessionId));
    const currentWorker = options.currentWorker ?? (options.sendStreamState ? undefined : currentRoutableWorker);
    const streamDispatcher = new TerminalStreamDispatcher({
      resolveRoute,
      sendStream: options.sendStreamState ?? sendTerminalStreamStateRequest,
      currentWorker,
    });
    const sendSnapshot = options.sendSnapshot
      ?? ((workerFp: string, sessionId: string, streamId: string) =>
        sendTerminalSnapshotRequest(workerFp, { sessionId, streamId }));

    this.streams = new TerminalViewStreamController({
      terminalScreen: options.terminalScreen,
      resolveRoute,
      streamDispatcher,
      currentWorker,
      createStreamDeadline: options.createStreamDeadline,
      sendSnapshot,
      geometries: (sessionId) => this.registry.geometries(sessionId),
      broadcast: (sessionId, status, message) => {
        this.registry.broadcast(sessionId, status, message);
      },
      closeViews: (sessionId) => this.registry.closeSession(sessionId),
      presence: (sessionId) => this.presence(sessionId),
      repairUnownedSession: (sessionId, streamId) => {
        this.relay.repairSession(sessionId, streamId);
      },
    });
    this.screen = this.streams.screen;
    this.registry = new TerminalViewRegistry({
      screen: this.screen,
      now: this.now,
      streamState: (sessionId) => this.streams.state(sessionId),
      recompute: (sessionId) => this.streams.recompute(sessionId),
      redrive: (sessionId) => this.streams.redrive(sessionId),
      onLiveViewExpired: (socketId, viewId, sessionId) => {
        this.onLiveViewExpired(socketId, viewId, sessionId);
      },
    });
    this.ownerForSession = options.ownerForSession ?? terminalViewOwnerForSession;
    this.relay = new TerminalViewOwnerRelay({
      screen: this.screen,
      socket: (socketId) => this.registrations.get(socketId),
      ownerForSession: (sessionId) => this.ownerForSession(sessionId),
      sendRelay: options.sendViewRelay ?? sendTerminalViewRelay,
      sendSocketClosed: options.sendViewSocketClosed ?? sendTerminalViewSocketClosed,
      sendSnapshot,
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
    this.registrations.set(registration.socketId, registration);
    this.registry.registerSocket(registration);
  }

  closeSocket(socketId: string): void {
    this.registry.closeSocket(socketId);
    this.registrations.delete(socketId);
    this.relay.closeSocket(socketId);
  }
  setOnLiveViewExpired(
    handler: ((socketId: string, viewId: string, sessionId: string) => void) | null,
  ): void {
    this.onLiveViewExpired = handler ?? (() => undefined);
  }

  /** A revoked device must stop reaching any owner before its sockets close,
   * so the relay registration goes first and the registry follows. */
  removeFingerprint(fingerprint: string): void {
    for (const [socketId, registration] of this.registrations) {
      if (registration.callerFingerprint === fingerprint) this.closeSocket(socketId);
    }
    this.registry.removeFingerprint(fingerprint);
  }

  /** The one gate: a session owned by a terminal-view-owner worker never
   * enters coordinator membership, so no view record, no recompute and no
   * stream desire can exist for it here. */
  handleViewCommand(socketId: string, command: TerminalViewCommand): void {
    const owner = this.ownerForSession(command.sessionId);
    if (owner === null) {
      this.registry.handleViewCommand(socketId, command);
      return;
    }
    this.relay.relayView(socketId, owner, command);
  }

  handleResync(socketId: string, command: TerminalResyncCommand): void {
    const owner = this.ownerForSession(command.sessionId);
    if (owner === null) {
      this.registry.handleResync(socketId, command);
      return;
    }
    this.relay.relayResync(socketId, owner, command);
  }

  applyOwnerViewState(workerFp: string, socketId: string, frame: TerminalViewStateFrame): void {
    this.relay.applyViewState(workerFp, socketId, frame);
  }

  publishPresence(sessionId: string): void {
    void this.presence(sessionId);
  }

  workerReplacement(workerFp: string): void {
    this.streams.workerReplacement(workerFp);
  }

  routeReconciled(workerFp: string, sessionIds: Iterable<string>): void {
    const owned: string[] = [];
    const legacy: string[] = [];
    for (const sessionId of sessionIds) {
      if (this.ownerForSession(sessionId) === workerFp) owned.push(sessionId);
      else legacy.push(sessionId);
    }
    if (legacy.length > 0) this.streams.routeReconciled(workerFp, legacy);
    // A legacy worker that upgraded in place leaves the coordinator holding
    // membership and a stream for its sessions; release exactly those. One it
    // never minimized keeps its screen replica, so a plain owner reconnect
    // costs watching browsers nothing.
    for (const sessionId of owned) {
      if (this.streams.state(sessionId) !== null) this.streams.closeSession(sessionId);
    }
  }

  workerRetired(workerFp: string, sessionIds: Iterable<string>): void {
    this.streams.workerRetired(workerFp, sessionIds);
  }

  closeSession(sessionId: string): void {
    dropTerminalViewProjection(sessionId);
    this.streams.closeSession(sessionId);
  }

  activeViewerFingerprints(sessionId: string): ReadonlySet<string> {
    return this.registry.activeViewerFingerprints(sessionId);
  }

  viewerProjection(): ReadonlyMap<string, ReadonlyMap<string, TerminalGeometry>> {
    return this.registry.viewerProjection();
  }

  viewerInputs(sessionId: string): readonly TerminalViewInput[] {
    return this.registry.viewerInputs(sessionId);
  }

  snapshot(sessionId: string): TerminalViewSnapshot | null {
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
    const viewers = ownerTerminalViewerGeometry(sessionId)
      ?? this.registry.viewerProjection().get(sessionId)
      ?? new Map();
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
