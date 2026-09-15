// Owns terminal view ownership and lifetime: which socket watches which
// session's terminal, lease renewal, parking on socket loss, and the sweep
// that drops leases and park graces. Command decisions live in
// terminal-view-registry-commands.ts; projections in
// terminal-view-registry-state.ts. Every mutation path must end in recompute()
// (geometry changed) or an explicit reply, never neither.

import type {
  TerminalResyncCommand, TerminalViewCommand, TerminalViewStatus,
} from "../gen/roost/v1/sync_pb.ts";
import { log } from "../log.ts";
import { isTerminalUuid, type TerminalGeometry } from "../viewport.ts";
import type { TerminalViewScreenPort } from "./screen-port.ts";
import {
  terminalViewKey, type TerminalViewStateSink,
} from "./terminal-view-protocol.ts";
import {
  activeTerminalFingerprints, projectTerminalViewInputs, projectTerminalViewers,
  terminalViewConstrains, terminalViewGeometrySet, terminalViewStats,
  type TerminalViewGeometrySet, type TerminalViewInput,
  type TerminalViewRecord as View,
  type TerminalViewSocketRecord as Socket,
  type TerminalViewTombstone as Tombstone,
} from "./terminal-view-registry-state.ts";
import { TerminalViewCommands } from "./terminal-view-registry-commands.ts";
import { TerminalViewRegistryOperations } from "./terminal-view-registry-operations.ts";

/** Why a session is not paintable right now, which decides what a rejoining
 * view is told: replay UNAVAILABLE, or redrive the host and let the next
 * heartbeat answer. */
export type TerminalUnavailablePolicy = "heartbeat" | "route" | "never";

/** The stream the host currently owns for one session. The registry only
 * reads it — coord's stream controller and the worker's view owner each mint
 * the stream and hand this shape back through streamState(). */
export interface TerminalStreamState {
  effective: TerminalGeometry | null;
  streamId: string;
  unavailable: boolean;
  unavailableReason: string;
  unavailablePolicy: TerminalUnavailablePolicy;
}

export interface TerminalViewRegistryOptions {
  screen: TerminalViewScreenPort;
  now(): number;
  streamState(sessionId: string): TerminalStreamState | null;
  recompute(sessionId: string): boolean;
  redrive(sessionId: string): void;
  onLiveViewExpired(socketId: string, viewId: string, sessionId: string): void;
}

export class TerminalViewRegistry {
  private readonly sockets = new Map<string, Socket>();
  private readonly views = new Map<string, View>();
  private readonly sessionViews = new Map<string, Set<string>>();
  private readonly tombstones = new Map<string, Tombstone>();
  private readonly operations: TerminalViewRegistryOperations;
  private readonly commands: TerminalViewCommands;

  constructor(private readonly options: TerminalViewRegistryOptions) {
    this.operations = new TerminalViewRegistryOperations(
      options,
      this.sockets,
      this.views,
      this.sessionViews,
      this.tombstones,
    );
    this.commands = new TerminalViewCommands(
      options,
      this.operations,
      this.sockets,
      this.views,
      this.sessionViews,
      this.tombstones,
    );
  }

  dispose(): void {
    for (const id of this.sockets.keys()) this.options.screen.unregisterSocket(id);
    this.sockets.clear();
    this.views.clear();
    this.sessionViews.clear();
    this.tombstones.clear();
  }

  registerSocket(registration: {
    socketId: string;
    viewerKey: string | null;
    callerFingerprint: string;
    allowsSession(sessionId: string): boolean;
    sink: TerminalViewStateSink;
  }): void {
    this.closeSocket(registration.socketId);
    this.sockets.set(registration.socketId, {
      id: registration.socketId,
      viewerKey: registration.viewerKey,
      fingerprint: registration.callerFingerprint,
      allowsSession: registration.allowsSession,
      sink: registration.sink,
      views: new Set(),
    });
    this.options.screen.registerSocket(registration.socketId, registration.sink);
  }

  closeSocket(socketId: string): void {
    const socket = this.sockets.get(socketId);
    if (!socket) return;
    this.sockets.delete(socketId);
    this.options.screen.unregisterSocket(socketId);
    const now = this.options.now();
    for (const key of socket.views) {
      const view = this.views.get(key);
      if (view?.socketId !== socketId) continue;
      view.parked = true;
      view.parkedAt = now;
    }
    // No recompute here on purpose: the park grace defers the geometry drop to
    // the sweep tick that observes it, so one owner decides when a parked
    // viewer stops constraining the PTY.
  }

  removeFingerprint(fingerprint: string): void {
    const affected = new Set<string>();
    for (const view of [...this.views.values()]) {
      if (view.fingerprint !== fingerprint) continue;
      affected.add(view.sessionId);
      this.operations.remove(view, false);
    }
    for (const [key, tombstone] of this.tombstones) {
      if (
        tombstone.viewerKey === fingerprint
        || tombstone.viewerKey.startsWith(`${fingerprint}:`)
      ) this.tombstones.delete(key);
    }
    for (const [id, socket] of this.sockets) {
      if (socket.fingerprint !== fingerprint) continue;
      this.sockets.delete(id);
      this.options.screen.unregisterSocket(id);
    }
    for (const sessionId of affected) this.options.recompute(sessionId);
  }

  handleViewCommand(socketId: string, command: TerminalViewCommand): void {
    this.commands.handle(socketId, command);
  }

  handleResync(socketId: string, command: TerminalResyncCommand): void {
    const socket = this.sockets.get(socketId);
    if (
      !socket?.viewerKey
      || !socket.allowsSession(command.sessionId)
      || !isTerminalUuid(command.viewId)
    ) return;
    const view = this.views.get(terminalViewKey(socket.viewerKey, command.viewId));
    if (
      !view
      || view.socketId !== socketId
      || view.parked
      || view.sessionId !== command.sessionId
    ) return;
    if (this.options.streamState(command.sessionId)?.streamId === command.streamId) {
      this.options.screen.resyncSocket(socketId, command.sessionId, {
        gridEpoch: command.gridEpoch,
        seq: command.seq,
      });
    }
  }

  closeSession(sessionId: string): void {
    for (const key of [...(this.sessionViews.get(sessionId) ?? [])]) {
      const view = this.views.get(key);
      if (view) this.operations.remove(view, false);
    }
    for (const [key, tombstone] of this.tombstones) {
      if (tombstone.intent.sessionId === sessionId) this.tombstones.delete(key);
    }
    this.sessionViews.delete(sessionId);
  }

  activeViewerFingerprints(sessionId: string): ReadonlySet<string> {
    return activeTerminalFingerprints(this.sessionViews.get(sessionId), this.views);
  }

  viewerProjection(): ReadonlyMap<string, ReadonlyMap<string, TerminalGeometry>> {
    return projectTerminalViewers(this.sessionViews, this.views);
  }

  geometries(sessionId: string): TerminalViewGeometrySet {
    return terminalViewGeometrySet(
      this.sessionViews.get(sessionId),
      this.views,
      this.options.now(),
    );
  }

  viewerInputs(sessionId: string): readonly TerminalViewInput[] {
    return projectTerminalViewInputs(
      this.sessionViews.get(sessionId),
      this.views,
      this.options.now(),
    );
  }

  viewStats(sessionId: string): { activeViews: number; parkedViews: number } {
    return terminalViewStats(this.sessionViews.get(sessionId), this.views);
  }

  broadcast(sessionId: string, status: TerminalViewStatus, message: string): void {
    for (const key of this.sessionViews.get(sessionId) ?? []) {
      const view = this.views.get(key);
      if (view && !view.parked) this.operations.replyView(view, status, message);
    }
  }

  sweep(): void {
    const now = this.options.now();
    const affected = new Set<string>();
    for (const view of [...this.views.values()]) {
      if (view.deadline > now) {
        // Re-minimize on the ONE tick a park grace lapses. Re-adding the
        // session every tick would redrive presence once a second per session
        // for as long as the record stays claimable.
        if (!view.constrains || terminalViewConstrains(view, now)) continue;
        view.constrains = false;
        affected.add(view.sessionId);
        log.info("terminal-view", "view_park_grace_lapsed", {
          session_id: view.sessionId,
          view_id: view.viewId,
          cols: view.cols,
          rows: view.rows,
        });
        continue;
      }
      if (!view.parked) {
        // Foreign synchronous callback: it closes the owning socket, which can
        // park sibling views re-entrantly. A throw here must not abandon the
        // rest of the sweep or kill the interval that owns every session's
        // geometry.
        try {
          this.options.onLiveViewExpired(view.socketId, view.viewId, view.sessionId);
        } catch (error) {
          log.error("terminal-view", "view_expiry_notify_failed", {
            session_id: view.sessionId,
            view_id: view.viewId,
            error: String(error),
          });
        }
      }
      affected.add(view.sessionId);
      this.operations.remove(view, true);
      this.operations.syncWatching(view.socketId, view.sessionId);
    }
    for (const [key, entry] of this.tombstones) {
      if (entry.expires <= now) this.tombstones.delete(key);
    }
    // Per session, so one session's failure can neither starve the others'
    // re-minimization nor stop the sweep timer for the whole process.
    for (const sessionId of affected) {
      try {
        this.options.recompute(sessionId);
      } catch (error) {
        log.error("terminal-view", "view_sweep_recompute_failed", {
          session_id: sessionId,
          error: String(error),
        });
      }
    }
  }

}
