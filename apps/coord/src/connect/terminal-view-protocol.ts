// Wire protocol helpers for terminal view commands and the state frames pushed
// to viewer sockets. validateTerminalViewCommand is the trust boundary: the
// viewerKey always comes from the authenticated socket, never from the command.
// Any new command field MUST be mirrored into TerminalViewIntent and
// equalTerminalViewIntent, or same-revision replays will silently diverge.
import { create } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema,
  TerminalViewStateFrameSchema,
  TerminalViewStatus,
  type TerminalViewCommand,
} from "@roost/shared/proto/sync_pb";
import {
  TERMINAL_MAX_COLS,
  TERMINAL_MAX_ROWS,
  isTerminalGeometry,
  isTerminalUuid,
} from "@roost/shared/viewport";
import type { TerminalScreenSocketSink } from "./terminal-screen-hub.ts";

const MAX_U64 = (1n << 64n) - 1n;

export interface TerminalViewIntent {
  sessionId: string;
  cols: number;
  rows: number;
  active: boolean;
}

export function terminalViewKey(viewerKey: string, viewId: string): string {
  return `${viewerKey}\0${viewId}`;
}

export function terminalViewIntent(command: TerminalViewCommand): TerminalViewIntent {
  return {
    sessionId: command.sessionId,
    cols: command.cols,
    rows: command.rows,
    active: command.active,
  };
}

export function equalTerminalViewIntent(
  left: TerminalViewIntent,
  right: TerminalViewIntent,
): boolean {
  return left.sessionId === right.sessionId
    && left.cols === right.cols
    && left.rows === right.rows
    && left.active === right.active;
}

export function validateTerminalViewCommand(
  viewerKey: string | null,
  command: TerminalViewCommand,
): string | null {
  if (!viewerKey) return "terminal views require a tab-bound Sync socket";
  if (!isTerminalUuid(command.viewId)) return "invalid terminal view id";
  if (!isTerminalUuid(command.sessionId)) return "invalid terminal session id";
  if (command.revision < 1n || command.revision > MAX_U64) {
    return "invalid terminal view revision";
  }
  if (command.active && !isTerminalGeometry(command)) {
    return "terminal geometry is outside 1..256";
  }
  if (
    !command.active
    && (
      !Number.isInteger(command.cols)
      || command.cols < 0
      || command.cols > TERMINAL_MAX_COLS
      || !Number.isInteger(command.rows)
      || command.rows < 0
      || command.rows > TERMINAL_MAX_ROWS
    )
  ) {
    return "inactive terminal geometry is outside 0..256";
  }
  return null;
}

interface TerminalViewState {
  viewId: string;
  sessionId: string;
  revision: bigint;
  active: boolean;
  streamId: string;
  status: TerminalViewStatus;
  effectiveCols: number;
  effectiveRows: number;
  message: string;
}

function terminalReason(value: string): string {
  const encoded = new TextEncoder().encode(value);
  return encoded.length <= 200
    ? value
    : new TextDecoder().decode(encoded.subarray(0, 200));
}

export function enqueueTerminalViewState(
  sink: TerminalScreenSocketSink,
  state: TerminalViewState,
): void {
  sink.enqueueTerminalState(create(FirehoseFrameSchema, {
    frame: {
      case: "terminalViewState",
      value: create(TerminalViewStateFrameSchema, {
        viewId: state.viewId,
        sessionId: state.sessionId,
        revision: state.revision,
        active: state.active,
        streamId: state.streamId,
        status: state.status,
        effectiveCols: state.effectiveCols,
        effectiveRows: state.effectiveRows,
        reason: terminalReason(state.message),
      }),
    },
  }), state.sessionId);
}

export function truncateTerminalReason(value: string): string {
  return terminalReason(value);
}
