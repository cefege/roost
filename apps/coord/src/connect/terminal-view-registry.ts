// Owns terminal view ownership: which socket watches which session's terminal,
// with lease deadlines renewed by every accepted command. A view bound to a
// live socket can be taken over ONLY when parked and the revision+intent match
// exactly; a view can never move between sessions. Every mutation path must
// end in recompute() (geometry changed) or an explicit reply, never neither.
import {
  TerminalViewStatus, type TerminalResyncCommand, type TerminalViewCommand,
} from "@roost/shared/proto/sync_pb";
import {
  TERMINAL_VIEW_LEASE_MS, isTerminalUuid, type TerminalGeometry,
} from "@roost/shared/viewport";
import { TerminalScreenHub, type TerminalScreenSocketSink } from "./terminal-screen-hub.ts";
import { enqueueTerminalViewState, equalTerminalViewIntent, terminalViewIntent,
  terminalViewKey, validateTerminalViewCommand, type TerminalViewIntent,
} from "./terminal-view-protocol.ts";
import {
  activeTerminalFingerprints, projectTerminalViewers, retainTerminalViewTombstone,
  terminalViewGeometries, terminalViewStats,
  type TerminalViewRecord as View, type TerminalViewSocketRecord as Socket,
  type TerminalViewTombstone as Tombstone,
} from "./terminal-view-registry-state.ts";
import type { TerminalStreamState } from "./terminal-view-stream-controller.ts";

const SOCKET_CAP = 64;
const SESSION_CAP = 256;

export interface TerminalViewRegistryOptions {
  screen: TerminalScreenHub;
  now(): number;
  streamState(sessionId: string): TerminalStreamState | null;
  recompute(sessionId: string): boolean;
  redrive(sessionId: string): void;
}

export class TerminalViewRegistry {
  private readonly sockets = new Map<string, Socket>();
  private readonly views = new Map<string, View>();
  private readonly sessionViews = new Map<string, Set<string>>();
  private readonly tombstones = new Map<string, Tombstone>();

  constructor(private readonly options: TerminalViewRegistryOptions) {}

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
    sink: TerminalScreenSocketSink;
  }): void {
    this.closeSocket(registration.socketId);
    this.sockets.set(registration.socketId, {
      id: registration.socketId,
      viewerKey: registration.viewerKey,
      fingerprint: registration.callerFingerprint,
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
    for (const key of socket.views) {
      const view = this.views.get(key);
      if (view?.socketId === socketId) view.parked = true;
    }
  }

  removeFingerprint(fingerprint: string): void {
    const affected = new Set<string>();
    for (const view of [...this.views.values()]) {
      if (view.fingerprint !== fingerprint) continue;
      affected.add(view.sessionId);
      this.remove(view, false);
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
    const socket = this.sockets.get(socketId);
    if (!socket) return;
    const invalid = validateTerminalViewCommand(socket.viewerKey, command);
    if (invalid) {
      this.replyCommand(socket, command, TerminalViewStatus.REJECTED, invalid, false);
      return;
    }
    const key = terminalViewKey(socket.viewerKey!, command.viewId);
    const intent = terminalViewIntent(command);
    const current = this.views.get(key);
    const old = this.tombstones.get(key);

    if (current && current.socketId !== socketId) {
      if (
        !current.parked
        || current.revision !== command.revision
        || !equalTerminalViewIntent(current, intent)
      ) {
        this.replyCommand(
          socket,
          command,
          TerminalViewStatus.REJECTED,
          "view is owned by another live socket",
          false,
        );
        return;
      }
      if (socket.views.size >= SOCKET_CAP) {
        this.replyCommand(
          socket,
          command,
          TerminalViewStatus.REJECTED,
          "terminal socket view capacity exceeded",
          false,
        );
        return;
      }
      current.socketId = socketId;
      current.parked = false;
      current.deadline = this.options.now() + TERMINAL_VIEW_LEASE_MS;
      socket.views.add(key);
      this.options.screen.setWatching(socketId, current.sessionId, true);
      if (this.options.streamState(current.sessionId)?.unavailable) {
        this.replayUnavailable(current);
      } else {
        this.replyView(current, TerminalViewStatus.ACCEPTED, "");
        this.options.screen.seedSocket(socketId, current.sessionId);
      }
      return;
    }

    if (current) {
      if (command.revision < current.revision) {
        this.replyCommand(socket, command, TerminalViewStatus.REJECTED, "stale terminal view revision", false);
        return;
      }
      if (command.revision === current.revision) {
        if (!equalTerminalViewIntent(current, intent)) {
          this.replyCommand(socket, command, TerminalViewStatus.REJECTED, "terminal view revision conflicts", false);
          return;
        }
        current.deadline = this.options.now() + TERMINAL_VIEW_LEASE_MS;
        if (this.options.streamState(current.sessionId)?.unavailable) {
          this.replayUnavailable(current);
        } else {
          this.replyView(current, TerminalViewStatus.ACCEPTED, "");
        }
        return;
      }
      if (command.sessionId !== current.sessionId) {
        this.replyCommand(socket, command, TerminalViewStatus.REJECTED, "a terminal view cannot change sessions", false);
        return;
      }
      if (!command.active) {
        this.remove(current, true, command.revision, intent);
        this.options.recompute(command.sessionId);
        this.replyCommand(socket, command, TerminalViewStatus.ACCEPTED, "", true);
        this.syncWatching(socketId, command.sessionId);
        return;
      }
      current.cols = command.cols;
      current.rows = command.rows;
      current.revision = command.revision;
      current.deadline = this.options.now() + TERMINAL_VIEW_LEASE_MS;
      if (!this.options.recompute(command.sessionId)) {
        this.replyView(current, TerminalViewStatus.ACCEPTED, "");
      }
      return;
    }

    if (old) {
      if (
        command.revision < old.revision
        || (command.revision === old.revision && !equalTerminalViewIntent(old.intent, intent))
      ) {
        this.replyCommand(socket, command, TerminalViewStatus.REJECTED, "stale or conflicting terminal view revision", false);
        return;
      }
      if (old.intent.sessionId !== command.sessionId) {
        this.replyCommand(socket, command, TerminalViewStatus.REJECTED, "a terminal view cannot change sessions", false);
        return;
      }
      if (command.revision === old.revision && !command.active) {
        this.replyCommand(socket, command, TerminalViewStatus.ACCEPTED, "", true);
        return;
      }
      this.tombstones.delete(key);
    }
    if (!command.active) {
      this.tombstone(key, socket.viewerKey!, command.revision, intent);
      this.replyCommand(socket, command, TerminalViewStatus.ACCEPTED, "", true);
      return;
    }
    if (socket.views.size >= SOCKET_CAP) {
      this.replyCommand(socket, command, TerminalViewStatus.REJECTED, "terminal socket view capacity exceeded", false);
      return;
    }
    let sessionViews = this.sessionViews.get(command.sessionId);
    if (!sessionViews) {
      sessionViews = new Set();
      this.sessionViews.set(command.sessionId, sessionViews);
    }
    if (sessionViews.size >= SESSION_CAP) {
      this.replyCommand(socket, command, TerminalViewStatus.REJECTED, "terminal session view capacity exceeded", false);
      return;
    }
    const view: View = {
      ...intent,
      key,
      viewId: command.viewId,
      viewerKey: socket.viewerKey!,
      fingerprint: socket.fingerprint,
      socketId,
      revision: command.revision,
      deadline: this.options.now() + TERMINAL_VIEW_LEASE_MS,
      parked: false,
    };
    this.views.set(key, view);
    socket.views.add(key);
    sessionViews.add(key);
    this.options.screen.setWatching(socketId, command.sessionId, true);
    if (!this.options.recompute(command.sessionId)) {
      this.replyView(view, TerminalViewStatus.ACCEPTED, "");
      this.options.screen.seedSocket(socketId, command.sessionId);
    }
  }

  handleResync(socketId: string, command: TerminalResyncCommand): void {
    const socket = this.sockets.get(socketId);
    if (!socket?.viewerKey || !isTerminalUuid(command.viewId)) return;
    const view = this.views.get(terminalViewKey(socket.viewerKey, command.viewId));
    if (
      !view
      || view.socketId !== socketId
      || view.parked
      || view.sessionId !== command.sessionId
    ) return;
    if (this.options.streamState(command.sessionId)?.streamId === command.streamId) {
      this.options.screen.invalidate(command.sessionId, "browser requested terminal rebaseline");
    }
  }

  closeSession(sessionId: string): void {
    for (const key of [...(this.sessionViews.get(sessionId) ?? [])]) {
      const view = this.views.get(key);
      if (view) this.remove(view, false);
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

  geometries(sessionId: string): readonly TerminalGeometry[] {
    return terminalViewGeometries(this.sessionViews.get(sessionId), this.views);
  }

  viewStats(sessionId: string): { activeViews: number; parkedViews: number } {
    return terminalViewStats(this.sessionViews.get(sessionId), this.views);
  }

  broadcast(sessionId: string, status: TerminalViewStatus, message: string): void {
    for (const key of this.sessionViews.get(sessionId) ?? []) {
      const view = this.views.get(key);
      if (view && !view.parked) this.replyView(view, status, message);
    }
  }

  sweep(): void {
    const now = this.options.now();
    const affected = new Set<string>();
    for (const view of [...this.views.values()]) {
      if (view.deadline > now) continue;
      affected.add(view.sessionId);
      this.remove(view, true);
      this.syncWatching(view.socketId, view.sessionId);
    }
    for (const [key, entry] of this.tombstones) {
      if (entry.expires <= now) this.tombstones.delete(key);
    }
    for (const sessionId of affected) this.options.recompute(sessionId);
  }

  private replayUnavailable(view: View): void {
    const session = this.options.streamState(view.sessionId);
    if (!session?.unavailable) {
      this.replyView(view, TerminalViewStatus.ACCEPTED, "");
      return;
    }
    if (session.unavailablePolicy === "heartbeat") {
      this.options.redrive(view.sessionId);
      return;
    }
    this.replyView(view, TerminalViewStatus.UNAVAILABLE, session.unavailableReason);
  }

  private replyView(view: View, status: TerminalViewStatus, message: string): void {
    const socket = this.sockets.get(view.socketId);
    const session = this.options.streamState(view.sessionId);
    if (!socket || !session?.effective) return;
    enqueueTerminalViewState(socket.sink, {
      viewId: view.viewId,
      sessionId: view.sessionId,
      revision: view.revision,
      active: true,
      streamId: session.streamId,
      status,
      effectiveCols: session.effective.cols,
      effectiveRows: session.effective.rows,
      message,
    });
  }

  private replyCommand(
    socket: Socket,
    command: TerminalViewCommand,
    status: TerminalViewStatus,
    message: string,
    inactive: boolean,
  ): void {
    enqueueTerminalViewState(socket.sink, {
      viewId: command.viewId,
      sessionId: command.sessionId,
      revision: command.revision,
      active: inactive ? false : command.active,
      streamId: "",
      status,
      effectiveCols: 0,
      effectiveRows: 0,
      message,
    });
  }

  private remove(
    view: View,
    save: boolean,
    revision = view.revision,
    intent: TerminalViewIntent = view,
  ): void {
    this.views.delete(view.key);
    this.sessionViews.get(view.sessionId)?.delete(view.key);
    this.sockets.get(view.socketId)?.views.delete(view.key);
    if (save) this.tombstone(view.key, view.viewerKey, revision, intent);
  }

  private tombstone(
    key: string,
    viewerKey: string,
    revision: bigint,
    intent: TerminalViewIntent,
  ): void {
    retainTerminalViewTombstone(
      this.tombstones,
      this.options.now(),
      key,
      viewerKey,
      revision,
      intent,
    );
  }

  private syncWatching(socketId: string, sessionId: string): void {
    const socket = this.sockets.get(socketId);
    if (!socket) return;
    let active = false;
    for (const key of socket.views) {
      if (this.views.get(key)?.sessionId === sessionId) active = true;
    }
    this.options.screen.setWatching(socketId, sessionId, active);
  }
}
