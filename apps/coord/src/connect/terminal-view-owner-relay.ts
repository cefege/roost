// The coordinator's whole path for a session whose worker owns its terminal
// views: authorize a browser view/resync command exactly as the coordinator
// registry would, then forward it instead of admitting local membership, and
// turn the worker's WTerminalViewState answer back into screen expectations
// and one browser frame. It holds no geometry and mints no stream — the worker
// is the only minimizer for these sessions. Constructed by terminal-view-hub.ts.

import { create } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema,
  TerminalViewStatus,
  type TerminalResyncCommand,
  type TerminalViewCommand,
  type TerminalViewStateFrame,
} from "@roost/shared/proto/sync_pb";
import {
  enqueueTerminalViewState,
  validateTerminalViewCommand,
  type TerminalViewStateSink,
} from "@roost/shared/terminal-view";
import { isTerminalUuid } from "@roost/shared/viewport";
import { diag } from "@roost/shared/diag";
import type { TerminalScreenHub } from "./terminal-screen-hub.ts";
import type {
  TerminalViewRelayCommand,
  TerminalViewRelayIdentity,
} from "./worker-send-terminal-view.ts";

/** The authenticated browser socket facts the relay needs. The hub's
 * TerminalSocketRegistration satisfies this structurally. */
export interface TerminalViewRelaySocket {
  viewerKey: string | null;
  callerFingerprint: string;
  allowsSession(sessionId: string): boolean;
  sink: TerminalViewStateSink;
}

export interface TerminalViewOwnerRelayOptions {
  screen: TerminalScreenHub;
  socket(socketId: string): TerminalViewRelaySocket | undefined;
  ownerForSession(sessionId: string): string | null;
  sendRelay(
    workerFp: string,
    identity: TerminalViewRelayIdentity,
    command: TerminalViewRelayCommand,
  ): boolean;
  sendSocketClosed(workerFp: string, socketId: string): boolean;
  /** Source-full repair for a stream the replica already expects. */
  sendSnapshot(workerFp: string, sessionId: string, streamId: string): boolean;
}

interface RelaySocketState {
  /** Owner-mode workers this socket has relayed to, so its close reaches them. */
  readonly workers: Set<string>;
  /** Session → the view ids the worker has confirmed as members on this
   * socket. Non-empty means the coordinator's screen replica must keep feeding
   * that session's cells here; it is bookkeeping for fan-out, never geometry. */
  readonly watches: Map<string, Set<string>>;
}

export class TerminalViewOwnerRelay {
  private readonly sockets = new Map<string, RelaySocketState>();

  constructor(private readonly options: TerminalViewOwnerRelayOptions) {}

  relayView(socketId: string, workerFp: string, command: TerminalViewCommand): void {
    const socket = this.options.socket(socketId);
    if (!socket) return;
    const invalid = validateTerminalViewCommand(socket.viewerKey, command);
    if (invalid) {
      this.refuse(socket, command, TerminalViewStatus.REJECTED, invalid);
      return;
    }
    if (!socket.allowsSession(command.sessionId)) {
      this.refuse(
        socket,
        command,
        TerminalViewStatus.REJECTED,
        "terminal session is unavailable",
      );
      return;
    }
    const admitted = this.options.sendRelay(
      workerFp,
      {
        socketId,
        viewerKey: socket.viewerKey!,
        deviceFingerprint: socket.callerFingerprint,
      },
      { case: "view", value: command },
    );
    if (!admitted) {
      this.refuse(
        socket,
        command,
        TerminalViewStatus.UNAVAILABLE,
        "terminal worker is unavailable",
      );
      return;
    }
    this.state(socketId).workers.add(workerFp);
  }

  /** Resync carries no reply of its own in the coordinator-owned path either:
   * a request the owner cannot honour is answered by the next state frame. */
  relayResync(socketId: string, workerFp: string, command: TerminalResyncCommand): void {
    const socket = this.options.socket(socketId);
    if (
      !socket?.viewerKey
      || !socket.allowsSession(command.sessionId)
      || !isTerminalUuid(command.viewId)
    ) return;
    const admitted = this.options.sendRelay(
      workerFp,
      {
        socketId,
        viewerKey: socket.viewerKey,
        deviceFingerprint: socket.callerFingerprint,
      },
      { case: "resync", value: command },
    );
    if (admitted) this.state(socketId).workers.add(workerFp);
  }

  /**
   * One view decision from the owning worker. The screen expectation is
   * installed BEFORE the browser sees the frame: the browser reacts to an
   * accepted stream id by applying whatever cells arrive next, and a replica
   * that has not yet been told to expect that stream would fold them against
   * the previous baseline.
   */
  applyViewState(workerFp: string, socketId: string, frame: TerminalViewStateFrame): void {
    const socket = this.options.socket(socketId);
    if (!socket) return;
    const sessionId = frame.sessionId;
    if (this.options.ownerForSession(sessionId) !== workerFp) {
      diag("terminal.view_state_foreign_worker", {
        session_id: sessionId,
        worker_fp: workerFp,
      });
      return;
    }
    if (!socket.allowsSession(sessionId)) return;
    // Only a membership reply carries a stream: a rejection or an inactive
    // acknowledgement leaves both empty and must not disturb the replica.
    const member = frame.streamId !== "";
    // Read before expectStream, which resets the session on a stream change.
    const previouslyExpected = this.options.screen.expectedStreamId(sessionId);
    if (member) {
      // TerminalScreenHub owns "already expecting this stream" — a repeat is a
      // no-op there, so the coordinator keeps no second seen-stream index.
      this.options.screen.expectStream(
        sessionId,
        frame.streamId,
        frame.effectiveCols,
        frame.effectiveRows,
      );
    }
    const { watching, attached } = this.track(socketId, sessionId, frame.viewId, member);
    this.options.screen.setWatching(socketId, sessionId, watching);
    // Only the decision that ATTACHES a socket to a session may seed it, the
    // same rule the coordinator registry's admit path follows: a lease
    // heartbeat re-declares the same view every few seconds, and seeding on
    // those would push a duplicate full to the browser on every beat.
    // seedSocket's boolean is "the replica served this socket", false exactly
    // when it holds no valid baseline. Going through the screen hub instead of
    // sendSnapshot reuses its one-outstanding-request throttle; a genuinely
    // new stream already has a baseline coming from the worker's own install.
    const seeded = attached && this.options.screen.seedSocket(socketId, sessionId);
    if (attached && !seeded && previouslyExpected === frame.streamId) {
      this.options.screen.invalidate(sessionId, "owner view attached without a replica baseline");
    }
    socket.sink.enqueueTerminalState(
      create(FirehoseFrameSchema, { frame: { case: "terminalViewState", value: frame } }),
      sessionId,
    );
  }

  closeSocket(socketId: string): void {
    const state = this.sockets.get(socketId);
    if (!state) return;
    this.sockets.delete(socketId);
    for (const workerFp of state.workers) this.options.sendSocketClosed(workerFp, socketId);
  }

  /** The replica lost this session's baseline and the coordinator owns no
   * stream for it, so the owning worker is the only source of a fresh full. */
  repairSession(sessionId: string, streamId: string): void {
    const workerFp = this.options.ownerForSession(sessionId);
    if (workerFp === null) return;
    this.options.sendSnapshot(workerFp, sessionId, streamId);
  }

  private refuse(
    socket: TerminalViewRelaySocket,
    command: TerminalViewCommand,
    status: TerminalViewStatus,
    message: string,
  ): void {
    enqueueTerminalViewState(socket.sink, {
      viewId: command.viewId,
      sessionId: command.sessionId,
      revision: command.revision,
      active: command.active,
      streamId: "",
      status,
      effectiveCols: 0,
      effectiveRows: 0,
      message,
    });
  }

  private state(socketId: string): RelaySocketState {
    let state = this.sockets.get(socketId);
    if (!state) {
      state = { workers: new Set(), watches: new Map() };
      this.sockets.set(socketId, state);
    }
    return state;
  }

  /** Records one view decision and reports whether the socket still holds any
   * confirmed view of the session, and whether this decision is the one that
   * newly attached it. */
  private track(
    socketId: string,
    sessionId: string,
    viewId: string,
    member: boolean,
  ): { watching: boolean; attached: boolean } {
    const state = this.state(socketId);
    let views = state.watches.get(sessionId);
    if (member) {
      const attached = views === undefined || views.size === 0;
      if (!views) {
        views = new Set();
        state.watches.set(sessionId, views);
      }
      views.add(viewId);
      return { watching: true, attached };
    }
    if (!views) return { watching: false, attached: false };
    views.delete(viewId);
    if (views.size > 0) return { watching: true, attached: false };
    state.watches.delete(sessionId);
    return { watching: false, attached: false };
  }
}
