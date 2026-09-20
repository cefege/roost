// Direct terminal input execution for LocalTerminalSockets.
// Both carriers share the live grant/route checks and input-work accounting here.
// A source-smoke-only peer hook pauses after authentication and before any write.
// The ordinary worker omits that hook, so this adds no public input control.

import type { InputCommand } from "@roost/shared/proto/sync_pb";
import { KEEPER_MAX_INPUT_BYTES } from "./keeper/protocol.ts";
import {
  directPortInputAuthority,
  directPortRequestBudget,
  type LocalTerminalAuthorizationDeps,
  type LocalTerminalPortSession,
} from "./local-terminal-socket-authority.ts";
import type { SessionManager } from "./session-manager.ts";
import type { WorkerInputResult } from "./session-terminal-control.ts";
import type { TerminalInputWorkBudget } from "./terminal-input-work-budget.ts";
import type { TerminalPacketPort } from "./terminal-packet-port.ts";

export type LocalTerminalPeerInputFault = (
  port: TerminalPacketPort,
  frame: InputCommand,
) => Promise<boolean>;


export type LocalTerminalPeerInputResultFault = (
  port: TerminalPacketPort,
  frame: InputCommand,
  result: WorkerInputResult,
) => boolean;
export interface LocalTerminalInputDeps {
  readonly session: LocalTerminalPortSession;
  readonly command: InputCommand;
  readonly sessions: () => SessionManager;
  readonly inputWorkBudget: TerminalInputWorkBudget;
  readonly authorizationDeps: LocalTerminalAuthorizationDeps;
  readonly isSessionAuthorized: (session: LocalTerminalPortSession, sessionId: string) => boolean;
  readonly sendResult: (command: InputCommand, result: WorkerInputResult) => void;
  readonly onAuthenticatedPeerInput?: LocalTerminalPeerInputFault;
  readonly shouldSendPeerInputResult?: LocalTerminalPeerInputResultFault;
}

/** Run one decoded input after an authenticated direct carrier hands it to the socket owner. */
export async function writeLocalTerminalInput(deps: LocalTerminalInputDeps): Promise<void> {
  const { session, command } = deps;
  if (session.expectedPeer && deps.onAuthenticatedPeerInput) {
    let allowed = false;
    try {
      allowed = await deps.onAuthenticatedPeerInput(session.port, command);
    } catch {
      // A failed smoke-only callback must fail closed before any keeper admission.
    }
    if (!allowed) {
      deps.sendResult(command, testHookRejectedInput());
      return;
    }
  }
  if (!deps.isSessionAuthorized(session, command.sessionId)) {
    deps.sendResult(command, unavailableInput());
    return;
  }
  if (command.data.byteLength > KEEPER_MAX_INPUT_BYTES) {
    deps.sendResult(command, oversizedInput());
    return;
  }
  const admission = deps.inputWorkBudget.reserveInput({
    origin: "direct",
    portId: session.port.socketId,
    byteLength: command.data.byteLength,
  });
  if (!admission.admitted) {
    deps.sendResult(command, {
      status: "rejected",
      writtenBytes: 0,
      reason: admission.reason,
    });
    return;
  }
  try {
    const result = await deps.sessions().writeTerminalInput(
      command.sessionId,
      command.inputSeq,
      command.data,
      directPortRequestBudget(deps.authorizationDeps.isCurrentPort, session),
      directPortInputAuthority(
        deps.authorizationDeps,
        session,
        command.sessionId,
        command.inputRouteEpoch,
      ),
    );
    if (
      session.expectedPeer
      && deps.shouldSendPeerInputResult
      && !deps.shouldSendPeerInputResult(session.port, command, result)
    ) return;
    deps.sendResult(command, result);
  } finally {
    admission.reservation.release();
  }
}

function unavailableInput(): WorkerInputResult {
  return {
    status: "rejected",
    writtenBytes: 0,
    reason: "terminal session is unavailable",
  };
}

function oversizedInput(): WorkerInputResult {
  return {
    status: "rejected",
    writtenBytes: 0,
    reason: "input exceeds 64 KiB",
  };
}

function testHookRejectedInput(): WorkerInputResult {
  return {
    status: "rejected",
    writtenBytes: 0,
    reason: "terminal input test hook rejected",
  };
}
