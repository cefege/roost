// The terminal view command state machine: what one client declaration does to
// membership. TerminalViewRegistry owns the maps, socket lifecycle and lease
// sweep and delegates every TerminalViewCommand here; this file decides accept,
// reject, reclaim and removal, then calls back through the shared operations
// and the registry's recompute hook. Split from the registry so the ownership
// rules live on one screen.

import {
  TerminalViewStatus, type TerminalViewCommand,
} from "@roost/shared/proto/sync_pb";
import { log } from "@roost/shared/log";
import { TERMINAL_SOCKET_VIEW_CAP, TERMINAL_VIEW_LEASE_MS } from "@roost/shared/viewport";
import {
  equalTerminalViewIntent, terminalViewIntent, terminalViewKey,
  validateTerminalViewCommand,
} from "./terminal-view-protocol.ts";
import type {
  TerminalViewRecord as View,
  TerminalViewSocketRecord as Socket,
  TerminalViewTombstone as Tombstone,
} from "./terminal-view-registry-state.ts";
import type { TerminalViewRegistryOperations } from "./terminal-view-registry-operations.ts";
import type { TerminalViewRegistryOptions } from "./terminal-view-registry.ts";

const SESSION_CAP = 256;

export class TerminalViewCommands {
  constructor(
    private readonly options: TerminalViewRegistryOptions,
    private readonly operations: TerminalViewRegistryOperations,
    private readonly sockets: Map<string, Socket>,
    private readonly views: Map<string, View>,
    private readonly sessionViews: Map<string, Set<string>>,
    private readonly tombstones: Map<string, Tombstone>,
  ) {}

  handle(socketId: string, command: TerminalViewCommand): void {
    const socket = this.sockets.get(socketId);
    if (!socket) return;
    const invalid = validateTerminalViewCommand(socket.viewerKey, command);
    if (invalid) {
      this.operations.replyCommand(socket, command, TerminalViewStatus.REJECTED, invalid, false);
      return;
    }
    // This is intentionally before view/tombstone/cache mutation. The set is
    // seeded from persisted sessions at Sync admission and only expanded by a
    // durable session event this socket owns.
    if (!socket.allowsSession(command.sessionId)) {
      this.operations.replyCommand(
        socket,
        command,
        TerminalViewStatus.REJECTED,
        "terminal session is unavailable",
        false,
      );
      return;
    }
    const key = terminalViewKey(socket.viewerKey!, command.viewId);
    const current = this.views.get(key);
    if (current && current.socketId !== socketId) {
      this.reclaim(socket, command, current, key);
      return;
    }
    if (current) {
      this.update(socket, command, current);
      return;
    }
    this.admit(socket, command, key);
  }

  /** A record whose socket is gone, reclaimed by the same tab on the same
   * device: viewerKey is `${fingerprint}:${tabId}`, so nobody else can reach
   * this key, and the previous owner's socket is provably closed. The incoming
   * geometry is ADOPTED — a phone that rotated or a laptop that resized while
   * offline must rejoin the aggregate now, not after the lease reaps it. */
  private reclaim(
    socket: Socket,
    command: TerminalViewCommand,
    current: View,
    key: string,
  ): void {
    if (!current.parked) {
      this.operations.replyCommand(
        socket,
        command,
        TerminalViewStatus.REJECTED,
        "view is owned by another live socket",
        false,
      );
      return;
    }
    if (command.revision < current.revision) {
      this.operations.replyCommand(
        socket,
        command,
        TerminalViewStatus.REJECTED,
        "stale terminal view revision",
        false,
      );
      return;
    }
    if (command.sessionId !== current.sessionId) {
      this.operations.replyCommand(
        socket,
        command,
        TerminalViewStatus.REJECTED,
        "a terminal view cannot change sessions",
        false,
      );
      return;
    }
    const intent = terminalViewIntent(command);
    if (!command.active) {
      this.operations.remove(current, true, command.revision, intent);
      this.options.recompute(command.sessionId);
      this.operations.replyCommand(socket, command, TerminalViewStatus.ACCEPTED, "", true);
      return;
    }
    if (socket.views.size >= TERMINAL_SOCKET_VIEW_CAP) {
      this.operations.replyCommand(
        socket,
        command,
        TerminalViewStatus.REJECTED,
        "terminal socket view capacity exceeded",
        false,
      );
      return;
    }
    const resized = current.cols !== command.cols || current.rows !== command.rows;
    current.socketId = socket.id;
    current.parked = false;
    current.parkedAt = 0;
    current.constrains = true;
    current.cols = command.cols;
    current.rows = command.rows;
    current.revision = command.revision;
    current.deadline = this.options.now() + TERMINAL_VIEW_LEASE_MS;
    socket.views.add(key);
    this.options.screen.setWatching(socket.id, current.sessionId, true);
    log.info("terminal-view", "view_reclaimed", {
      session_id: current.sessionId,
      view_id: current.viewId,
      socket_id: socket.id,
      cols: current.cols,
      rows: current.rows,
      resized,
    });
    if (this.options.recompute(current.sessionId)) return;
    if (this.options.streamState(current.sessionId)?.unavailable) {
      this.operations.replayUnavailable(current);
      return;
    }
    this.operations.replyView(current, TerminalViewStatus.ACCEPTED, "");
    this.options.screen.seedSocket(socket.id, current.sessionId);
  }

  private update(socket: Socket, command: TerminalViewCommand, current: View): void {
    if (command.revision < current.revision) {
      this.operations.replyCommand(socket, command, TerminalViewStatus.REJECTED, "stale terminal view revision", false);
      return;
    }
    const intent = terminalViewIntent(command);
    if (command.revision === current.revision) {
      // A same-revision command from the owning socket PROVES liveness even
      // when its geometry is refused, so renew before judging the intent:
      // dropping the renewal expires the lease and sync-ws closes the whole
      // socket, parking every other session that socket was watching.
      current.deadline = this.options.now() + TERMINAL_VIEW_LEASE_MS;
      if (!equalTerminalViewIntent(current, intent)) {
        log.warn("terminal-view", "view_intent_conflict_renewed", {
          session_id: current.sessionId,
          view_id: current.viewId,
          socket_id: socket.id,
          revision: current.revision.toString(),
        });
        this.operations.replyCommand(socket, command, TerminalViewStatus.REJECTED, "terminal view revision conflicts", false);
        return;
      }
      if (this.options.streamState(current.sessionId)?.unavailable) {
        this.operations.replayUnavailable(current);
      } else {
        this.operations.replyView(current, TerminalViewStatus.ACCEPTED, "");
      }
      if (this.options.screen.ensureSocketStream(socket.id, current.sessionId)) {
        this.options.screen.seedSocket(socket.id, current.sessionId);
      }
      return;
    }
    if (command.sessionId !== current.sessionId) {
      this.operations.replyCommand(socket, command, TerminalViewStatus.REJECTED, "a terminal view cannot change sessions", false);
      return;
    }
    if (!command.active) {
      this.operations.remove(current, true, command.revision, intent);
      this.options.recompute(command.sessionId);
      this.operations.replyCommand(socket, command, TerminalViewStatus.ACCEPTED, "", true);
      this.operations.syncWatching(socket.id, command.sessionId);
      return;
    }
    current.cols = command.cols;
    current.rows = command.rows;
    current.revision = command.revision;
    current.deadline = this.options.now() + TERMINAL_VIEW_LEASE_MS;
    if (!this.options.recompute(command.sessionId)) {
      this.operations.replyView(current, TerminalViewStatus.ACCEPTED, "");
    }
  }

  private admit(socket: Socket, command: TerminalViewCommand, key: string): void {
    const intent = terminalViewIntent(command);
    const old = this.tombstones.get(key);
    if (old) {
      if (
        command.revision < old.revision
        || (command.revision === old.revision && !equalTerminalViewIntent(old.intent, intent))
      ) {
        this.operations.replyCommand(socket, command, TerminalViewStatus.REJECTED, "stale or conflicting terminal view revision", false);
        return;
      }
      if (old.intent.sessionId !== command.sessionId) {
        this.operations.replyCommand(socket, command, TerminalViewStatus.REJECTED, "a terminal view cannot change sessions", false);
        return;
      }
      if (command.revision === old.revision && !command.active) {
        this.operations.replyCommand(socket, command, TerminalViewStatus.ACCEPTED, "", true);
        return;
      }
      this.tombstones.delete(key);
    }
    if (!command.active) {
      this.operations.tombstone(key, socket.viewerKey!, command.revision, intent);
      this.operations.replyCommand(socket, command, TerminalViewStatus.ACCEPTED, "", true);
      return;
    }
    if (socket.views.size >= TERMINAL_SOCKET_VIEW_CAP) {
      this.operations.replyCommand(socket, command, TerminalViewStatus.REJECTED, "terminal socket view capacity exceeded", false);
      return;
    }
    let sessionViews = this.sessionViews.get(command.sessionId);
    if (!sessionViews) {
      sessionViews = new Set();
      this.sessionViews.set(command.sessionId, sessionViews);
    }
    if (sessionViews.size >= SESSION_CAP) {
      this.operations.replyCommand(socket, command, TerminalViewStatus.REJECTED, "terminal session view capacity exceeded", false);
      return;
    }
    const view: View = {
      ...intent,
      key,
      viewId: command.viewId,
      viewerKey: socket.viewerKey!,
      fingerprint: socket.fingerprint,
      socketId: socket.id,
      revision: command.revision,
      deadline: this.options.now() + TERMINAL_VIEW_LEASE_MS,
      parked: false,
      parkedAt: 0,
      constrains: true,
    };
    this.views.set(key, view);
    socket.views.add(key);
    sessionViews.add(key);
    this.options.screen.setWatching(socket.id, command.sessionId, true);
    if (!this.options.recompute(command.sessionId)) {
      this.operations.replyView(view, TerminalViewStatus.ACCEPTED, "");
      this.options.screen.seedSocket(socket.id, command.sessionId);
    }
  }
}
