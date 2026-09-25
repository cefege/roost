// Source-worker side of the disposable terminal peer fault control socket.
// The alternate smoke entrypoint creates this client and injects its state into
// runWorker; ordinary source and compiled workers never import this module.
// Commands mutate only in-process test state and are discarded on disconnect.

import { createConnection, type Socket } from "node:net";
import type { SessionManager } from "../../apps/worker/src/session/session-manager.ts";
import {
  TerminalPeerTestFaultState,
  type TerminalPeerOfferFault,
} from "../../apps/worker/src/terminal/peer/terminal-peer-test-faults.ts";
import type {
  PeerFaultCommand,
  PeerFaultCommandAction,
} from "./stack-peer-fault-control.ts";

const MAX_CONTROL_BYTES = 16 * 1024;

interface WorkerPeerFaultClient {
  setSessions(sessions: SessionManager): void;
  dispose(): void;
}

/** Connect once during source-worker boot. Parent teardown closes this before the worker. */
export async function startWorkerPeerFaultClient(
  socketPath: string,
  state: TerminalPeerTestFaultState,
): Promise<WorkerPeerFaultClient> {
  const workerLabel = process.env.ROOST_WORKER_LABEL;
  if (!workerLabel) throw new Error("terminal peer fault worker has no label");
  const socket = await connectUnixSocket(socketPath);
  let sessions: SessionManager | undefined;
  let buffered = "";
  let disposed = false;
  socket.setEncoding("utf8");
  writeJson(socket, { type: "hello", workerLabel });
  socket.on("data", (chunk: string) => {
    if (disposed || buffered.length + chunk.length > MAX_CONTROL_BYTES) {
      socket.destroy();
      return;
    }
    buffered += chunk;
    for (;;) {
      const newline = buffered.indexOf("\n");
      if (newline < 0) return;
      const line = buffered.slice(0, newline);
      buffered = buffered.slice(newline + 1);
      const command = parseCommand(line);
      if (!command) {
        socket.destroy();
        return;
      }
      void dispatchCommand(command, state, () => sessions).then(
        (value) => { writeJson(socket, { type: "result", requestId: command.requestId, ok: true, value }); },
        (error: unknown) => {
          writeJson(socket, {
            type: "result",
            requestId: command.requestId,
            ok: false,
            error: error instanceof Error ? error.message : String(error),
          });
        },
      );
    }
  });
  socket.on("error", () => { socket.destroy(); });
  return {
    setSessions: (next) => { sessions = next; },
    dispose: () => {
      if (disposed) return;
      disposed = true;
      state.dispose();
      socket.destroy();
    },
  };
}

async function dispatchCommand(
  command: PeerFaultCommand,
  state: TerminalPeerTestFaultState,
  sessions: () => SessionManager | undefined,
): Promise<unknown> {
  switch (command.action) {
    case "set_packet_blackhole":
      state.setPacketBlackhole(booleanPayload(command.payload));
      return null;
    case "drop_next_input_result":
      state.dropNextPeerInputResult();
      return null;
    case "advance_grant_clock":
      state.advanceGrantClock(numberPayload(command.payload));
      return null;
    case "shrink_grant_session":
      return state.shrinkGrantForSession(stringPayload(command.payload));
    case "hold_history_response":
      return await state.holdNextHistoryResponse(stringPayload(command.payload));
    case "release_history_response":
      state.releaseHistoryResponse(stringPayload(command.payload), true);
      return null;
    case "drop_history_response":
      state.releaseHistoryResponse(stringPayload(command.payload), false);
      return null;
    case "inject_malformed_packet":
      state.injectMalformedPacket(malformedPacketPayload(command.payload));
      return null;
    case "set_history_paused":
      state.setHistoryDeliveryPaused(booleanPayload(command.payload));
      return null;
    case "hold_keeper_admission":
      return await state.holdKeeperAdmission(requireSessions(sessions), stringPayload(command.payload));
    case "release_keeper_admission":
      state.releaseKeeperAdmission(stringPayload(command.payload));
      return null;
    case "drop_next_direct_retire":
      state.armDirectRetireDrop();
      return null;
    case "arm_offer_fault":
      state.armOfferFault(offerFaultPayload(command.payload));
      return null;
  }
}

function requireSessions(readSessions: () => SessionManager | undefined): SessionManager {
  const sessions = readSessions();
  if (!sessions) throw new Error("terminal peer fault worker has not finished session boot");
  return sessions;
}

function booleanPayload(value: unknown): boolean {
  if (typeof value !== "boolean") throw new Error("terminal peer fault requires a boolean payload");
  return value;
}

function numberPayload(value: unknown): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value <= 0) {
    throw new Error("terminal peer fault requires a positive safe integer payload");
  }
  return value;
}

function stringPayload(value: unknown): string {
  if (typeof value !== "string" || value.length === 0 || value.length > 128) {
    throw new Error("terminal peer fault requires a bounded string payload");
  }
  return value;
}

function offerFaultPayload(value: unknown): TerminalPeerOfferFault {
  if (
    value === "invalid_sdp"
    || value === "missing_grant"
    || value === "expired_grant"
    || value === "identity_mismatch"
  ) return value;
  throw new Error("terminal peer fault offer kind is invalid");
}

function malformedPacketPayload(value: unknown): "offset" | "total" | "id" {
  if (value === "offset" || value === "total" || value === "id") return value;
  throw new Error("terminal peer malformed packet kind is invalid");
}

function parseCommand(value: string): PeerFaultCommand | null {
  try {
    const parsed: unknown = JSON.parse(value);
    if (typeof parsed !== "object" || parsed === null) return null;
    const command = parsed as Record<string, unknown>;
    if (
      command.type !== "command"
      || typeof command.requestId !== "string"
      || !isCommandAction(command.action)
    ) return null;
    return {
      type: "command",
      requestId: command.requestId,
      action: command.action,
      payload: command.payload,
    };
  } catch {
    return null;
  }
}

function isCommandAction(value: unknown): value is PeerFaultCommandAction {
  return value === "set_packet_blackhole"
    || value === "drop_next_input_result"
    || value === "advance_grant_clock"
    || value === "shrink_grant_session"
    || value === "hold_history_response"
    || value === "release_history_response"
    || value === "drop_history_response"
    || value === "hold_keeper_admission"
    || value === "release_keeper_admission"
    || value === "inject_malformed_packet"
    || value === "set_history_paused"
    || value === "drop_next_direct_retire"
    || value === "arm_offer_fault";
}

function writeJson(socket: Socket, value: unknown): void {
  if (socket.destroyed || !socket.writable) return;
  try {
    socket.write(`${JSON.stringify(value)}\n`);
  } catch {
    socket.destroy();
  }
}

function connectUnixSocket(socketPath: string): Promise<Socket> {
  return new Promise<Socket>((resolve, reject) => {
    const socket = createConnection({ path: socketPath });
    const fail = (error: Error): void => {
      socket.off("connect", ready);
      reject(error);
    };
    const ready = (): void => {
      socket.off("error", fail);
      resolve(socket);
    };
    socket.once("error", fail);
    socket.once("connect", ready);
  });
}
