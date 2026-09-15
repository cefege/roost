// The local terminal fast path. When the worker that owns a PTY also served
// this page, its loopback socket carries that session's view commands, input,
// cell frames and scrollback directly — the coordinator is not in the path, so
// an open pane keeps painting while the coordinator is down. Inbound frames
// enter the SAME replica and view-state owners Sync uses; this module owns the
// socket, its generation fence and the grant it presents.

import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { diag } from "@roost/shared/diag";
import {
  LocalTerminalClientFrameSchema,
  LocalTerminalServerFrameSchema,
  type LocalScrollbackResponse,
  type LocalTerminalClientFrame,
  type LocalTerminalServerFrame,
} from "@roost/shared/proto/local_terminal_pb";
import type {
  TerminalResyncCommand,
  TerminalViewCommand,
} from "@roost/shared/proto/sync_pb";
import { backoffDelayMs } from "@roost/shared/retry";
import { readLocalBootstrap } from "../lib/localBootstrap.ts";
import { registerSyncV2GenerationHandler } from "../store/sync.ts";
import {
  dispatchTerminalCellChunk,
  dispatchTerminalCellFrame,
  dispatchTerminalViewState,
} from "../store/terminal-stream.ts";
import {
  LOCAL_TERMINAL_PROCESS_EPOCH,
  notifyTerminalLocalTransportChanged,
  registerTerminalLocalTransport,
  type LocalScrollbackQuery,
} from "../store/terminal-stream-transport.ts";
import type { TerminalGenerationToken } from "../store/terminal-stream-types.ts";
import {
  armLocalTerminalGrantRenewal,
  clearLocalTerminalGrantRetry,
  currentLocalTerminalGrant,
  dropLocalTerminalGrant,
  noteLocalTerminalViewPublished,
  refreshLocalTerminalGrant,
  resetLocalTerminalGrants,
  _releaseLocalTerminalGrantRenewalForTest,
  type LocalTerminalGrant,
  type LocalTerminalGrantSocket,
} from "./local-terminal-grants.ts";
import {
  failLocalRequests,
  requestLocalScrollbackCells as requestScrollbackOverLink,
  sendLocalInput,
  settleLocalInput,
  settleLocalScrollback,
  type LocalTerminalLink,
} from "./local-terminal-requests.ts";
import type { InputAdmission } from "./terminal-input-lanes.ts";

// The worker's own door, declared in apps/worker/src/local-ui-server.ts. The
// page IS that origin, so the host comes from the document instead of a pinned
// port — harness workers bind reserved random ports.
const LOCAL_TERMINAL_PATH = "/ws/local-terminal";
const LOCAL_TERMINAL_SUBPROTOCOL = "roost-local-terminal";
const REDIAL_BASE_MS = 500;
const REDIAL_MAX_MS = 8_000;


interface ReadySocket {
  sessionIds: Set<string>;
  generation: bigint;
  token: TerminalGenerationToken;
}

let installed = false;
let socket: WebSocket | null = null;
let ready: ReadySocket | null = null;
let dialAttempt = 0;
let redialTimer: ReturnType<typeof setTimeout> | null = null;

const link: LocalTerminalLink = {
  ownsSession: isLocalTerminalSession,
  generation: () => ready?.generation ?? null,
  send: sendClientFrame,
};

const grantSocket: LocalTerminalGrantSocket = {
  connected: () => socket !== null,
  present: presentGrant,
};

/** Install the fast path for this document. Without a worker-served bootstrap
 * there is no local worker to talk to and nothing is dialed. */
export function startLocalTerminalFastPath(): void {
  if (installed) return;
  installed = true;
  registerTerminalLocalTransport({
    ownsSession: isLocalTerminalSession,
    noteViewPublished,
    generationToken: () => ready?.token ?? null,
    publishView: publishLocalTerminalView,
    publishResync: publishLocalTerminalResync,
    sendInput: sendLocalTerminalInput,
    requestScrollback: requestLocalScrollbackCells,
    redial: redialLocalTerminal,
    reset: resetLocalTerminalState,
  });
  const bootstrap = readLocalBootstrap();
  if (!bootstrap) return;
  diag("local_terminal.available", { worker_fp: bootstrap.workerFingerprint });
  // Every Sync (re)connect is a fresh chance to mint or renew a grant, and the
  // coordinator returning is exactly when a pane stranded on Sync can take the
  // fast path it could not be granted while the coordinator was gone.
  registerSyncV2GenerationHandler((state) => {
    if (!state?.ready) return;
    clearLocalTerminalGrantRetry();
    void refreshLocalTerminalGrant(grantSocket, "sync_connected");
  });
}

export function localTerminalWorkerFingerprint(): string | null {
  return readLocalBootstrap()?.workerFingerprint ?? null;
}

/** Granted by the coordinator AND acknowledged by a ready local socket. */
export function isLocalTerminalSession(sessionId: string): boolean {
  return ready?.sessionIds.has(sessionId) ?? false;
}

/** A ready socket holding at least one granted session: local panes keep
 * painting even while the coordinator is unreachable. */
export function localTerminalSessionsLive(): boolean {
  return (ready?.sessionIds.size ?? 0) > 0;
}

export function publishLocalTerminalView(command: TerminalViewCommand): boolean {
  if (!isLocalTerminalSession(command.sessionId)) return false;
  return sendClientFrame({ case: "terminalView", value: command });
}

export function publishLocalTerminalResync(command: TerminalResyncCommand): boolean {
  if (!isLocalTerminalSession(command.sessionId)) return false;
  return sendClientFrame({ case: "terminalResync", value: command });
}

export function sendLocalTerminalInput(
  sessionId: string,
  bytes: Uint8Array,
  viewId?: string,
): InputAdmission {
  return sendLocalInput(link, sessionId, bytes, viewId);
}

export function requestLocalScrollbackCells(
  query: LocalScrollbackQuery,
): Promise<LocalScrollbackResponse> {
  return requestScrollbackOverLink(link, query);
}

/** A credential boundary retires the grant along with the credential that
 * earned it; a new device identity must be granted again. */
export function resetLocalTerminalState(reason: string): void {
  resetLocalTerminalGrants();
  closeSocket(reason);
}

export function _resetLocalTerminalForTest(): void {
  resetLocalTerminalState("test reset");
  _releaseLocalTerminalGrantRenewalForTest();
  installed = false;
  dialAttempt = 0;
}

function noteViewPublished(sessionId: string): void {
  noteLocalTerminalViewPublished(grantSocket, sessionId);
}

function presentGrant(grant: LocalTerminalGrant): void {
  closeSocket("grant replaced");
  dialAttempt = 0;
  dialSocket(grant);
}

function dialSocket(grant = currentLocalTerminalGrant()): void {
  if (!grant || socket || typeof location === "undefined") return;
  const scheme = location.protocol === "https:" ? "wss:" : "ws:";
  let dialed: WebSocket;
  try {
    dialed = new WebSocket(
      `${scheme}//${location.host}${LOCAL_TERMINAL_PATH}`,
      LOCAL_TERMINAL_SUBPROTOCOL,
    );
  } catch (error) {
    diag("local_terminal.dial_failed", { error: String(error) });
    scheduleRedial();
    return;
  }
  dialed.binaryType = "arraybuffer";
  socket = dialed;
  dialed.onopen = (): void => {
    if (socket !== dialed) return;
    sendClientFrame({
      case: "hello",
      value: {
        $typeName: "roost.v1.LocalTerminalHello",
        grantId: grant.grantId,
        secret: grant.secret,
        tabId: grant.tabId,
        deviceFingerprint: grant.deviceFingerprint,
      },
    });
  };
  dialed.onmessage = (event: MessageEvent): void => {
    if (socket !== dialed || !(event.data instanceof ArrayBuffer)) return;
    handleServerFrame(new Uint8Array(event.data));
  };
  dialed.onclose = (): void => {
    if (socket !== dialed) return;
    socket = null;
    retireReadySocket("local terminal socket closed");
    scheduleRedial();
  };
  dialed.onerror = (): void => {
    if (socket === dialed) diag("local_terminal.socket_error", {});
  };
}

function handleServerFrame(bytes: Uint8Array): void {
  let frame: LocalTerminalServerFrame;
  try {
    frame = fromBinary(LocalTerminalServerFrameSchema, bytes);
  } catch (error) {
    diag("local_terminal.frame_undecodable", { error: String(error) });
    return;
  }
  const oneof = frame.frame;
  const current = ready;
  switch (oneof.case) {
    case "ready":
      admitReadySocket(
        oneof.value.workerFingerprint,
        oneof.value.sessionIds,
        oneof.value.socketGeneration,
      );
      return;
    case "terminalViewState":
      if (current) dispatchTerminalViewState(oneof.value, current.token);
      return;
    case "cellGrid":
      if (current) dispatchTerminalCellFrame(oneof.value, current.token);
      return;
    case "cellGridChunk":
      if (current) dispatchTerminalCellChunk(oneof.value, current.token);
      return;
    case "inputAccepted":
    case "inputRejected":
    case "inputAmbiguous":
      if (current) settleLocalInput(oneof.case, oneof.value, current.generation);
      return;
    case "scrollback":
      settleLocalScrollback(oneof.value);
      return;
    case "closed":
      diag("local_terminal.closed", { reason: oneof.value.reason });
      // Only the coordinator can replace a refused grant, so drop it instead of
      // redialing with a credential the worker just told us it will not take.
      dropLocalTerminalGrant();
      closeSocket(oneof.value.reason);
      return;
    default:
      return;
  }
}

function admitReadySocket(
  workerFingerprint: string,
  sessionIds: string[],
  generation: bigint,
): void {
  const expected = readLocalBootstrap()?.workerFingerprint;
  if (!expected || workerFingerprint !== expected) {
    diag("local_terminal.identity_mismatch", {
      advertised: expected ?? null,
      observed: workerFingerprint,
    });
    dropLocalTerminalGrant();
    closeSocket("worker identity mismatch");
    return;
  }
  dialAttempt = 0;
  ready = {
    sessionIds: new Set(sessionIds),
    generation,
    token: {
      socketGeneration: Number(generation),
      socketId: `local:${generation}`,
      processEpoch: LOCAL_TERMINAL_PROCESS_EPOCH,
      domainGeneration: generation,
    },
  };
  diag("local_terminal.ready", {
    worker_fp: workerFingerprint,
    sessions: sessionIds.length,
    socket_generation: generation.toString(),
  });
  armLocalTerminalGrantRenewal(grantSocket);
  notifyTerminalLocalTransportChanged();
}

function retireReadySocket(reason: string): void {
  const had = ready !== null;
  ready = null;
  failLocalRequests(reason);
  if (!had) return;
  diag("local_terminal.retired", { reason });
  notifyTerminalLocalTransportChanged();
}

function redialLocalTerminal(reason: string): boolean {
  if (!currentLocalTerminalGrant()) return false;
  diag("local_terminal.redial", { reason });
  closeSocket(reason);
  dialAttempt = 0;
  dialSocket();
  return true;
}

function closeSocket(reason: string): void {
  const closing = socket;
  socket = null;
  if (redialTimer !== null) {
    clearTimeout(redialTimer);
    redialTimer = null;
  }
  if (closing) {
    closing.onopen = null;
    closing.onmessage = null;
    closing.onclose = null;
    closing.onerror = null;
    try {
      closing.close();
    } catch {
      // A socket that refuses to close is already unusable.
    }
  }
  retireReadySocket(reason);
}

function scheduleRedial(): void {
  if (!currentLocalTerminalGrant() || redialTimer !== null || socket) return;
  const delay = backoffDelayMs(dialAttempt++, { baseMs: REDIAL_BASE_MS, maxMs: REDIAL_MAX_MS });
  redialTimer = setTimeout(() => {
    redialTimer = null;
    dialSocket();
  }, delay);
}

function sendClientFrame(frame: LocalTerminalClientFrame["frame"]): boolean {
  const open = socket;
  if (!open || open.readyState !== WebSocket.OPEN) return false;
  try {
    open.send(toBinary(
      LocalTerminalClientFrameSchema,
      create(LocalTerminalClientFrameSchema, { frame }),
    ));
    return true;
  } catch (error) {
    diag("local_terminal.send_failed", { case: frame.case ?? null, error: String(error) });
    return false;
  }
}
