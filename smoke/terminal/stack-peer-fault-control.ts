// Parent-side control server for disposable source-worker terminal peer faults.
// Its Unix socket exists only under a stack temp root and accepts no product path.
// A source wrapper identifies its worker label, receives bounded commands, and is
// destroyed with the stack so fault state cannot survive a test lifecycle.

import { chmodSync, rmSync } from "node:fs";
import { createServer, type Server, type Socket } from "node:net";
import { join } from "node:path";
import { randomUUID } from "node:crypto";

const MAX_CONTROL_BYTES = 16 * 1024;
const WORKER_CONNECT_TIMEOUT_MS = 30_000;
const COMMAND_TIMEOUT_MS = 15_000;

export type PeerFaultOfferKind =
  | "invalid_sdp"
  | "missing_grant"
  | "expired_grant"
  | "identity_mismatch";

export type PeerFaultMalformedPacketKind = "offset" | "total" | "id";

export type PeerFaultCommandAction =
  | "set_packet_blackhole"
  | "drop_next_input_result"
  | "advance_grant_clock"
  | "shrink_grant_session"
  | "hold_history_response"
  | "release_history_response"
  | "drop_history_response"
  | "inject_malformed_packet"
  | "set_history_paused"
  | "hold_keeper_admission"
  | "release_keeper_admission"
  | "drop_next_direct_retire"
  | "arm_offer_fault";

export type PeerFaultCommand = {
  readonly type: "command";
  readonly requestId: string;
  readonly action: PeerFaultCommandAction;
  readonly payload: unknown;
};

type PeerFaultWorkerHello = { readonly type: "hello"; readonly workerLabel: string };
type PeerFaultWorkerResult = {
  readonly type: "result";
  readonly requestId: string;
  readonly ok: boolean;
  readonly value?: unknown;
  readonly error?: string;
};

type ConnectedWorker = {
  readonly label: string;
  readonly socket: Socket;
  readonly pending: Set<string>;
};

type PendingCommand = {
  readonly worker: ConnectedWorker;
  readonly resolve: (value: unknown) => void;
  readonly reject: (error: Error) => void;
  readonly timer: NodeJS.Timeout;
};

export interface StackPeerFaultControl {
  readonly socketPath: string;
  command<T>(workerLabel: string, action: PeerFaultCommandAction, payload?: unknown): Promise<T>;
  stop(): Promise<void>;
}

/** Start before source workers boot; each requested command waits for its named wrapper. */
export async function startStackPeerFaultControl(root: string): Promise<StackPeerFaultControl> {
  const socketPath = join(root, "terminal-peer-fault-control.sock");
  try { rmSync(socketPath); } catch { /* a fresh stack root has no old control socket */ }
  const workers = new Map<string, ConnectedWorker>();
  const pending = new Map<string, PendingCommand>();
  const sockets = new Set<Socket>();
  let stopped = false;
  let stopPromise: Promise<void> | undefined;
  const server = createServer((socket) => {
    sockets.add(socket);
    let buffered = "";
    let worker: ConnectedWorker | null = null;
    socket.setEncoding("utf8");
    socket.on("data", (chunk: string) => {
      if (buffered.length + chunk.length > MAX_CONTROL_BYTES) {
        socket.destroy();
        return;
      }
      buffered += chunk;
      for (;;) {
        const newline = buffered.indexOf("\n");
        if (newline < 0) return;
        const line = buffered.slice(0, newline);
        buffered = buffered.slice(newline + 1);
        const message = parseWorkerMessage(line);
        if (!message) {
          socket.destroy();
          return;
        }
        if (message.type === "hello") {
          if (worker || stopped) {
            socket.destroy();
            return;
          }
          const prior = workers.get(message.workerLabel);
          prior?.socket.destroy();
          worker = { label: message.workerLabel, socket, pending: new Set() };
          workers.set(worker.label, worker);
          continue;
        }
        if (!worker) {
          socket.destroy();
          return;
        }
        settleWorkerResult(worker, message, pending);
      }
    });
    socket.on("close", () => {
      sockets.delete(socket);
      if (worker && workers.get(worker.label) === worker) workers.delete(worker.label);
      if (worker) rejectWorkerCommands(worker, pending, "terminal peer fault worker disconnected");
    });
    socket.on("error", () => { socket.destroy(); });
  });
  await listenUnixServer(server, socketPath);
  chmodSync(socketPath, 0o600);

  return {
    socketPath,
    command: async <T>(
      workerLabel: string,
      action: PeerFaultCommandAction,
      payload: unknown = null,
    ): Promise<T> => {
      const worker = await waitForWorker(workers, workerLabel, () => stopped);
      if (stopped) throw new Error("terminal peer fault control is stopped");
      if (worker.socket.destroyed || !worker.socket.writable) {
        throw new Error(`terminal peer fault worker ${workerLabel} is unavailable`);
      }
      const requestId = randomUUID();
      return await new Promise<T>((resolve, reject) => {
        const timer = setTimeout(() => {
          pending.delete(requestId);
          worker.pending.delete(requestId);
          reject(new Error(`terminal peer fault command ${action} timed out`));
        }, COMMAND_TIMEOUT_MS);
        timer.unref?.();
        const resolveUnknown = (value: unknown): void => { resolve(value as T); };
        pending.set(requestId, { worker, resolve: resolveUnknown, reject, timer });
        worker.pending.add(requestId);
        if (!writeJson(worker.socket, { type: "command", requestId, action, payload } satisfies PeerFaultCommand)) {
          clearTimeout(timer);
          pending.delete(requestId);
          worker.pending.delete(requestId);
          reject(new Error(`terminal peer fault worker ${workerLabel} rejected a command`));
        }
      });
    },
    stop: async () => {
      stopPromise ??= new Promise<void>((resolve) => {
        stopped = true;
        for (const worker of workers.values()) worker.socket.destroy();
        for (const socket of sockets) socket.destroy();
        rejectAllCommands(pending, "terminal peer fault control stopped");
        try {
          server.close(() => resolve());
        } catch {
          resolve();
        }
      }).finally(() => {
        try { rmSync(socketPath); } catch { /* socket unlinked with its server */ }
      });
      await stopPromise;
    },
  };
}

function settleWorkerResult(
  worker: ConnectedWorker,
  result: PeerFaultWorkerResult,
  pending: Map<string, PendingCommand>,
): void {
  const command = pending.get(result.requestId);
  if (!command || command.worker !== worker) return;
  pending.delete(result.requestId);
  command.worker.pending.delete(result.requestId);
  clearTimeout(command.timer);
  if (result.ok) command.resolve(result.value);
  else command.reject(new Error(result.error || "terminal peer fault command failed"));
}

function rejectWorkerCommands(
  worker: ConnectedWorker,
  pending: Map<string, PendingCommand>,
  message: string,
): void {
  for (const requestId of worker.pending) {
    const command = pending.get(requestId);
    if (!command) continue;
    pending.delete(requestId);
    clearTimeout(command.timer);
    command.reject(new Error(message));
  }
  worker.pending.clear();
}

function rejectAllCommands(pending: Map<string, PendingCommand>, message: string): void {
  for (const command of pending.values()) {
    clearTimeout(command.timer);
    command.reject(new Error(message));
  }
  pending.clear();
}

async function waitForWorker(
  workers: ReadonlyMap<string, ConnectedWorker>,
  workerLabel: string,
  stopped: () => boolean,
): Promise<ConnectedWorker> {
  const existing = workers.get(workerLabel);
  if (existing && !existing.socket.destroyed && existing.socket.writable) return existing;
  const deadline = Date.now() + WORKER_CONNECT_TIMEOUT_MS;
  while (!stopped() && Date.now() < deadline) {
    await Bun.sleep(25);
    const worker = workers.get(workerLabel);
    if (worker && !worker.socket.destroyed && worker.socket.writable) return worker;
  }
  throw new Error(`terminal peer fault worker ${workerLabel} did not connect`);
}

function parseWorkerMessage(value: string): PeerFaultWorkerHello | PeerFaultWorkerResult | null {
  try {
    const parsed: unknown = JSON.parse(value);
    if (typeof parsed !== "object" || parsed === null) return null;
    const message = parsed as Record<string, unknown>;
    if (message.type === "hello" && validString(message.workerLabel)) {
      return { type: "hello", workerLabel: message.workerLabel };
    }
    if (message.type === "result" && validString(message.requestId) && typeof message.ok === "boolean") {
      return {
        type: "result",
        requestId: message.requestId,
        ok: message.ok,
        value: message.value,
        error: typeof message.error === "string" ? message.error : undefined,
      };
    }
    return null;
  } catch {
    return null;
  }
}

function validString(value: unknown): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= 128;
}

function writeJson(socket: Socket, value: unknown): boolean {
  try {
    socket.write(`${JSON.stringify(value)}\n`);
    return true;
  } catch {
    socket.destroy();
    return false;
  }
}

function listenUnixServer(server: Server, socketPath: string): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    const fail = (error: Error): void => {
      server.off("listening", ready);
      reject(error);
    };
    const ready = (): void => {
      server.off("error", fail);
      resolve();
    };
    server.once("error", fail);
    server.once("listening", ready);
    server.listen(socketPath);
  });
}
