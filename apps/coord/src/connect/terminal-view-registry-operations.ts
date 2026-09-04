// Applies terminal-view replies, tombstones, and shared index mutations for
// TerminalViewRegistry. Keeping these operations together ensures every removal
// updates the view, session, and socket indexes before a tombstone or watch-state
// change can be observed by the screen hub.

import {
  TerminalViewStatus,
  type TerminalResyncCommand,
  type TerminalViewCommand,
} from "@roost/shared/proto/sync_pb";
import { isTerminalUuid } from "@roost/shared/viewport";
import {
  enqueueTerminalViewState,
  terminalViewKey,
  type TerminalViewIntent,
} from "./terminal-view-protocol.ts";
import {
  retainTerminalViewTombstone,
  type TerminalViewRecord as View,
  type TerminalViewSocketRecord as Socket,
  type TerminalViewTombstone as Tombstone,
} from "./terminal-view-registry-state.ts";
import type { TerminalViewRegistryOptions } from "./terminal-view-registry.ts";

// Dashboard scope is part of ownership because one browser tab can retain its
// viewer and view ids while switching dashboards.
export function scopedTerminalViewKey(
  dashboardId: string,
  viewerKey: string,
  viewId: string,
): string {
  return terminalViewKey(`${dashboardId}\u0000${viewerKey}`, viewId);
}

export class TerminalViewRegistryOperations {
  constructor(
    private readonly options: TerminalViewRegistryOptions,
    private readonly sockets: Map<string, Socket>,
    private readonly views: Map<string, View>,
    private readonly sessionViews: Map<string, Set<string>>,
    private readonly tombstones: Map<string, Tombstone>,
  ) {}

  handleResync(socketId: string, command: TerminalResyncCommand): void {
    const socket = this.sockets.get(socketId);
    if (
      !socket?.viewerKey
      || !socket.allowsSession(command.sessionId)
      || !isTerminalUuid(command.viewId)
    ) return;
    const view = this.views.get(scopedTerminalViewKey(
      socket.dashboardId,
      socket.viewerKey,
      command.viewId,
    ));
    if (
      !view
      || view.dashboardId !== socket.dashboardId
      || view.socketId !== socketId
      || view.parked
      || view.sessionId !== command.sessionId
    ) return;
    if (this.options.streamState(command.sessionId)?.streamId === command.streamId) {
      this.options.screen.resyncSocket(socketId, command.sessionId);
    }
  }

  replayUnavailable(view: View): void {
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

  replyView(view: View, status: TerminalViewStatus, message: string): void {
    const socket = this.sockets.get(view.socketId);
    const session = this.options.streamState(view.sessionId);
    if (!socket || socket.dashboardId !== view.dashboardId || !session?.effective) return;
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

  replyCommand(
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

  remove(
    view: View,
    save: boolean,
    revision = view.revision,
    intent: TerminalViewIntent = view,
  ): void {
    this.views.delete(view.key);
    this.sessionViews.get(view.sessionId)?.delete(view.key);
    this.sockets.get(view.socketId)?.views.delete(view.key);
    if (save) {
      this.tombstone(
        view.key,
        view.viewerKey,
        view.dashboardId,
        revision,
        intent,
      );
    }
  }

  tombstone(
    key: string,
    viewerKey: string,
    dashboardId: string,
    revision: bigint,
    intent: TerminalViewIntent,
  ): void {
    retainTerminalViewTombstone(
      this.tombstones,
      this.options.now(),
      key,
      viewerKey,
      dashboardId,
      revision,
      intent,
    );
  }

  syncWatching(socketId: string, sessionId: string): void {
    const socket = this.sockets.get(socketId);
    if (!socket) return;
    let active = false;
    for (const key of socket.views) {
      if (this.views.get(key)?.sessionId === sessionId) active = true;
    }
    this.options.screen.setWatching(socketId, sessionId, active);
  }
}
