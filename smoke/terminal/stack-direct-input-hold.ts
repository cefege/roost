// Source-worker-only direct input hold for terminal handoff smoke proofs.
// A disposable Unix socket injects an in-process callback into a smoke worker;
// it holds one authenticated peer input without exposing a product command or env.
// Stack teardown resolves the callback as a discard before it stops its worker.

import { createConnection, createServer, type Server, type Socket } from "node:net";
import { rmSync } from "node:fs";
import { join } from "node:path";

const MAX_CONTROL_BYTES = 8 * 1024;
const DIRECT_INPUT_HOLD_TIMEOUT_MS = 15_000;

type InputHoldRequest = {
  readonly type: "input";
  readonly sessionId: string;
  readonly inputSeq: string;
};

type InputHoldDecision = { readonly action: "pass" | "release" | "drop" };

type PendingInputHold = {
  readonly sessionId: string;
  readonly resolve: (hold: HeldDirectInput) => void;
  readonly reject: (error: Error) => void;
};

type CapturedInputHold = {
  readonly socket: Socket;
  readonly sessionId: string;
  readonly inputSeq: string;
};

export interface HeldDirectInput {
  readonly sessionId: string;
  readonly inputSeq: bigint;
  release(): void;
  drop(): void;
}

export interface DirectInputHold {
  readonly socketPath: string;
  holdNextInput(sessionId: string): Promise<HeldDirectInput>;
  stop(): Promise<void>;
}

/** Start the stack-owned control endpoint before a source worker begins booting. */
export async function startDirectInputHold(root: string): Promise<DirectInputHold> {
  const socketPath = join(root, "terminal-direct-input-hold.sock");
  try { rmSync(socketPath); } catch { /* a fresh stack root normally has no prior socket */ }
  const sockets = new Set<Socket>();
  let pending: PendingInputHold | null = null;
  let captured: CapturedInputHold | null = null;
  let stopped = false;
  let stopPromise: Promise<void> | undefined;
  let server: Server;

  const settleCaptured = (current: CapturedInputHold, action: "release" | "drop"): void => {
    if (captured !== current) return;
    captured = null;
    writeDecision(current.socket, { action });
  };

  const receive = (socket: Socket, message: InputHoldRequest): void => {
    if (stopped) {
      writeDecision(socket, { action: "drop" });
      return;
    }
    const currentPending = pending;
    if (!currentPending || currentPending.sessionId !== message.sessionId || captured !== null) {
      writeDecision(socket, { action: "pass" });
      return;
    }
    pending = null;
    const current: CapturedInputHold = {
      socket,
      sessionId: message.sessionId,
      inputSeq: message.inputSeq,
    };
    captured = current;
    currentPending.resolve({
      sessionId: current.sessionId,
      inputSeq: BigInt(current.inputSeq),
      release: () => { settleCaptured(current, "release"); },
      drop: () => { settleCaptured(current, "drop"); },
    });
  };

  server = createServer((socket) => {
    sockets.add(socket);
    let buffered = "";
    socket.setEncoding("utf8");
    socket.on("data", (chunk: string) => {
      if (stopped || buffered.length + chunk.length > MAX_CONTROL_BYTES) {
        socket.destroy();
        return;
      }
      buffered += chunk;
      const newline = buffered.indexOf("\n");
      if (newline < 0) return;
      const line = buffered.slice(0, newline);
      buffered = "";
      const message = parseInputHoldRequest(line);
      if (!message) {
        socket.destroy();
        return;
      }
      receive(socket, message);
    });
    socket.on("close", () => {
      sockets.delete(socket);
      if (captured?.socket === socket) captured = null;
    });
    socket.on("error", () => { socket.destroy(); });
  });
  await listenUnixServer(server, socketPath);

  return {
    socketPath,
    holdNextInput: (sessionId) => {
      if (stopped) return Promise.reject(new Error("direct input hold is stopped"));
      if (sessionId.length === 0) return Promise.reject(new Error("direct input hold requires a session ID"));
      if (pending !== null || captured !== null) {
        return Promise.reject(new Error("direct input hold already owns an input"));
      }
      return new Promise<HeldDirectInput>((resolve, reject) => {
        pending = { sessionId, resolve, reject };
      });
    },
    stop: async () => {
      stopPromise ??= new Promise<void>((resolve) => {
        stopped = true;
        const currentPending = pending;
        pending = null;
        currentPending?.reject(new Error("direct input hold stopped before input arrived"));
        if (captured) settleCaptured(captured, "drop");
        for (const socket of sockets) socket.destroy();
        try {
          server.close(() => resolve());
        } catch {
          resolve();
        }
      }).finally(() => {
        try { rmSync(socketPath); } catch { /* socket may already be gone after server close */ }
      });
      await stopPromise;
    },
  };
}

/** Worker-side callback client. A missing or malformed harness decision never writes the PTY. */
export function awaitDirectInputHoldDecision(
  socketPath: string,
  sessionId: string,
  inputSeq: bigint,
): Promise<boolean> {
  return new Promise<boolean>((resolve) => {
    let settled = false;
    const socket = createConnection({ path: socketPath });
    const settle = (allowed: boolean): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      socket.destroy();
      resolve(allowed);
    };
    const timeout = setTimeout(() => { settle(false); }, DIRECT_INPUT_HOLD_TIMEOUT_MS);
    timeout.unref?.();
    let buffered = "";
    socket.setEncoding("utf8");
    socket.on("connect", () => {
      const request: InputHoldRequest = { type: "input", sessionId, inputSeq: inputSeq.toString() };
      socket.write(`${JSON.stringify(request)}\n`);
    });
    socket.on("data", (chunk: string) => {
      buffered += chunk;
      if (buffered.length > MAX_CONTROL_BYTES) return settle(false);
      const newline = buffered.indexOf("\n");
      if (newline < 0) return;
      const decision = parseInputHoldDecision(buffered.slice(0, newline));
      settle(decision?.action !== "drop");
    });
    socket.on("error", () => { settle(false); });
    socket.on("close", () => { settle(false); });
  });
}

function writeDecision(socket: Socket, decision: InputHoldDecision): void {
  if (socket.destroyed || !socket.writable) return;
  try {
    socket.end(`${JSON.stringify(decision)}\n`);
  } catch {
    socket.destroy();
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

function parseInputHoldRequest(line: string): InputHoldRequest | null {
  try {
    const parsed: unknown = JSON.parse(line);
    if (
      typeof parsed !== "object"
      || parsed === null
      || !hasStringField(parsed, "type", "input")
      || !hasStringField(parsed, "sessionId")
      || !hasStringField(parsed, "inputSeq")
      || !/^\d+$/.test(parsed.inputSeq)
    ) return null;
    return { type: "input", sessionId: parsed.sessionId, inputSeq: parsed.inputSeq };
  } catch {
    return null;
  }
}

function parseInputHoldDecision(line: string): InputHoldDecision | null {
  try {
    const parsed: unknown = JSON.parse(line);
    if (
      typeof parsed !== "object"
      || parsed === null
      || !hasStringField(parsed, "action")
      || (parsed.action !== "pass" && parsed.action !== "release" && parsed.action !== "drop")
    ) return null;
    return { action: parsed.action };
  } catch {
    return null;
  }
}

function hasStringField(
  value: object,
  key: string,
  expected?: string,
): value is Record<string, string> {
  if (!(key in value) || typeof (value as Record<string, unknown>)[key] !== "string") return false;
  return expected === undefined || (value as Record<string, string>)[key] === expected;
}
