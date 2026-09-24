// Loopback direct-terminal adapter and its one-shot local-door lifecycle.
// Each ready socket is an immutable registry connection; a replacement gets a
// fresh browser namespace only for rolling workers without epoch/socket fields.
// Grants remain worker-scoped in local-terminal-grants.ts.

import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { diag } from "@roost/observability/diag";
import {
  LocalScrollbackRequestSchema,
  LocalTerminalClientFrameSchema,
  LocalTerminalHelloSchema,
  LocalTerminalServerFrameSchema,
  type LocalScrollbackResponse,
  type LocalTerminalClientFrame,
  type LocalTerminalServerFrame,
} from "@roost/protocol/proto/local_terminal_pb";
import {
  TerminalTransportProbeSchema,
  type InputCommand,
  type TerminalInputRouteClaim,
  type TerminalInputRouteResult,
  type TerminalResyncCommand,
  type TerminalTransportProbeResult,
  type TerminalViewCommand,
} from "@roost/protocol/proto/sync_pb";
import { backoffDelayMs } from "@roost/protocol/retry";
import {
  discoverLocalWorkerDoor,
  readLocalWorkerDoor,
  registerLocalWorkerDoorHandler,
  type LocalWorkerDoor,
} from "../../client/carriers/localWorkerDiscovery.ts";
import { dispatchDirectTerminalFrame } from "../terminal-stream-promotion.ts";
import { registerSyncV2GenerationHandler } from "../sync.ts";
import {
  terminalDirectRegistry,
  type LocalScrollbackQuery,
  type TerminalDirectConnection,
  type TerminalDirectRegistryEvent,
} from "../terminal-stream-transport.ts";
import type { TerminalGenerationToken } from "../terminal-stream-types.ts";
import {
  clearTerminalGrantRetry,
  currentTerminalGrant,
  dropTerminalGrant,
  refreshTerminalGrant,
  resetTerminalGrants,
  setTerminalGrantDemand,
  subscribeTerminalGrant,
  _releaseTerminalGrantRenewalForTest,
  type LocalTerminalGrant,
} from "./local-terminal-grants.ts";
import { createTerminalDirectRequestId } from "../../client/carriers/terminal-direct-browser.ts";
import { TerminalPeerProbeState } from "../../client/carriers/terminal-peer-probe-state.ts";
import { retireTerminalInput, settleTerminalInput } from "./terminal-input-router.ts";
import { resetTerminalPeerState, retireTerminalDirectConnection, stageTerminalDirectConnection, startTerminalPeerFastPath } from "./terminal-peer.ts";

const LOCAL_TERMINAL_PATH = "/ws/local-terminal";
const LOCAL_TERMINAL_SUBPROTOCOL = "roost-local-terminal";
const REDIAL_BASE_MS = 500;
const REDIAL_MAX_MS = 8_000;
const SCROLLBACK_TIMEOUT_MS = 15_000; const ROUTE_CLAIM_TIMEOUT_MS = 8_000;
type ClientFrame = LocalTerminalClientFrame["frame"]; interface Deferred<T> { readonly resolve: (value: T) => void; readonly reject: (error: Error) => void; readonly timer: Timer; }
interface LoopbackConnectionOptions {
  readonly door: LocalWorkerDoor;
  readonly grant: LocalTerminalGrant;
  readonly onReady: (connection: LoopbackTerminalConnection) => void;
  readonly onClosed: (connection: LoopbackTerminalConnection, reason: string) => void;
  readonly onGrantRejected: () => void;
}
/** One loopback WebSocket generation. It is registered only after authenticated Ready. */
export class LoopbackTerminalConnection implements TerminalDirectConnection {
  readonly kind = "loopback" as const;
  readonly connectionId = createTerminalDirectRequestId();
  readonly workerFp: string;
  private socket: WebSocket | null = null;
  private tokenValue: TerminalGenerationToken | null = null;
  private workerEpochValue = "";
  private inputRoutes = false;
  private ended = false;
  private readonly readySessions = new Set<string>();
  private readonly scrollbackWaiters = new Map<string, Deferred<LocalScrollbackResponse>>(); private readonly routeWaiters = new Map<string, Deferred<TerminalInputRouteResult>>();
  private probes: TerminalPeerProbeState | null = null;
  constructor(private readonly options: LoopbackConnectionOptions) { this.workerFp = options.door.workerFingerprint; }
  get workerEpoch(): string { return this.workerEpochValue; } get inputRouteSupported(): boolean { return this.inputRoutes; }
  token(): TerminalGenerationToken | null { return this.tokenValue; }
  allowsSession(sessionId: string): boolean { return this.tokenValue !== null && this.readySessions.has(sessionId); }
  get sessionCount(): number { return this.readySessions.size; }
  updateGrant(grant: LocalTerminalGrant): boolean {
    if (!this.tokenValue || grant.workerFp !== this.workerFp || (grant.workerEpoch && grant.workerEpoch !== this.workerEpochValue) || [...this.readySessions].some((sessionId) => !grant.sessionIds.includes(sessionId))) return false;
    for (const sessionId of grant.sessionIds) this.readySessions.add(sessionId);
    this.inputRoutes &&= grant.inputRouteSupported; return true;
  }
  telemetry() {
    const probe = this.probes?.telemetry();
    return { opaquePeerId: null, lastProbeAtMs: probe?.lastProbeAtMs ?? null, rttMs: probe?.rttMs ?? null, livenessQualified: !this.ended && this.tokenValue !== null, bufferedBytes: this.ended ? null : this.socket?.bufferedAmount ?? null };
  }
  start(): void {
    if (this.ended || this.socket || typeof WebSocket === "undefined") return;
    const url = new URL(LOCAL_TERMINAL_PATH, this.options.door.origin);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    let socket: WebSocket;
    try { socket = new WebSocket(url.toString(), LOCAL_TERMINAL_SUBPROTOCOL); }
    catch (error) { diag("local_terminal.dial_failed", { error: String(error) }); return this.finish("local terminal dial failed"); }
    socket.binaryType = "arraybuffer";
    socket.onopen = () => {
      if (this.socket !== socket) return;
      this.sendFrame({ case: "hello", value: create(LocalTerminalHelloSchema, {
        grantId: this.options.grant.grantId, secret: this.options.grant.secret, tabId: this.options.grant.tabId,
        deviceFingerprint: this.options.grant.deviceFingerprint, peerId: "", workerEpoch: "",
      }) });
    };
    socket.onmessage = (event) => {
      if (this.socket !== socket || !(event.data instanceof ArrayBuffer)) return;
      this.receiveFrame(new Uint8Array(event.data));
    };
    socket.onclose = () => this.finish("local terminal socket closed");
    socket.onerror = () => { if (this.socket === socket) diag("local_terminal.socket_error", {}); };
    this.socket = socket;
  }
  publishView(command: TerminalViewCommand): boolean {
    return this.allowsSession(command.sessionId) && this.sendFrame({ case: "terminalView", value: command });
  }
  publishResync(command: TerminalResyncCommand): boolean {
    return this.allowsSession(command.sessionId) && this.sendFrame({ case: "terminalResync", value: command });
  }
  sendInput(command: InputCommand): "accepted" | "refused" {
    return this.allowsSession(command.sessionId) && this.sendFrame({ case: "input", value: command }) ? "accepted" : "refused";
  }
  claimInputRoute(command: TerminalInputRouteClaim): Promise<TerminalInputRouteResult> {
    if (!this.inputRoutes || command.workerEpoch !== this.workerEpoch) return Promise.reject(new Error("local terminal input route is unavailable"));
    return this.waitForResponse(this.routeWaiters, command.requestId, ROUTE_CLAIM_TIMEOUT_MS, "local terminal input route claim timed out", () =>
      this.sendFrame({ case: "inputRouteClaim", value: command }));
  }
  requestScrollback(query: LocalScrollbackQuery): Promise<LocalScrollbackResponse> {
    if (!this.allowsSession(query.sessionId)) return Promise.reject(new Error("local terminal is not authorized for this session"));
    const requestId = createTerminalDirectRequestId();
    return this.waitForResponse(this.scrollbackWaiters, requestId, SCROLLBACK_TIMEOUT_MS, "local terminal scrollback request timed out", () =>
      this.sendFrame({ case: "scrollback", value: create(LocalScrollbackRequestSchema, { ...query, requestId }) }));
  }
  probe(requestId: string): Promise<void> {
    const probes = this.probes;
    if (!this.inputRoutes || !probes || !requestId) return Promise.reject(new Error("local terminal probe is unavailable"));
    return probes.start(requestId, () => this.sendFrame({
      case: "transportProbe",
      value: create(TerminalTransportProbeSchema, { requestId, workerFp: this.workerFp }),
    }));
  }
  close(reason: string): void {
    const socket = this.socket;
    this.socket = null;
    if (socket) {
      socket.onopen = null;
      socket.onmessage = null;
      socket.onclose = null;
      socket.onerror = null;
      try { socket.close(); } catch { /* already unusable */ }
    }
    this.finish(reason);
  }
  private receiveFrame(bytes: Uint8Array): void {
    let frame: LocalTerminalServerFrame;
    try { frame = fromBinary(LocalTerminalServerFrameSchema, bytes); }
    catch (error) { diag("local_terminal.frame_undecodable", { error: String(error) }); return this.close("local terminal frame was invalid"); }
    const oneof = frame.frame;
    if (!this.tokenValue) {
      if (oneof.case !== "ready") return this.close("local terminal required Ready first");
      return this.admitReady(oneof.value);
    }
    if (oneof.case === "ready") return this.close("local terminal repeated Ready frame");
    const token = this.tokenValue;
    if (oneof.case === "inputAccepted" || oneof.case === "inputRejected" || oneof.case === "inputAmbiguous") {
      settleTerminalInput(token, {
        sessionId: oneof.value.sessionId,
        inputSeq: oneof.value.inputSeq,
        status: oneof.case === "inputAccepted" ? "accepted" : oneof.case === "inputRejected" ? "rejected" : "ambiguous",
        writtenBytes: oneof.case === "inputRejected" ? undefined : oneof.value.writtenBytes,
        reason: oneof.case === "inputAccepted" ? undefined : oneof.value.reason,
      });
      return;
    }
    if (oneof.case === "inputRouteResult") return this.resolveRouteWaiter(oneof.value);
    if (oneof.case === "transportProbeResult") return this.resolveProbeWaiter(oneof.value);
    if (oneof.case === "scrollback") return this.resolveWaiter(this.scrollbackWaiters, oneof.value.requestId, oneof.value);
    if (oneof.case === "closed") {
      this.options.onGrantRejected();
      return this.close(oneof.value.reason || "local terminal closed");
    }
    dispatchDirectTerminalFrame(token, frame);
  }
  private admitReady(ready: Extract<LocalTerminalServerFrame["frame"], { case: "ready" }>["value"]): void {
    const generation = Number(ready.socketGeneration);
    const currentEpoch = ready.workerEpoch;
    const currentSocketId = ready.socketId;
    const compatibility = !currentEpoch && !currentSocketId && !ready.peerId;
    if (
      this.options.grant.workerFp !== this.workerFp || ready.workerFingerprint !== this.workerFp
      || !!ready.peerId || !Number.isSafeInteger(generation)
      || ready.sessionIds.some((sessionId) => !this.options.grant.sessionIds.includes(sessionId))
      || (!compatibility && (!currentEpoch || !currentSocketId))
      || (!!this.options.grant.workerEpoch && !!currentEpoch && currentEpoch !== this.options.grant.workerEpoch)
    ) return this.close("local terminal Ready did not match its authenticated grant");
    this.workerEpochValue = compatibility ? this.connectionId : currentEpoch;
    this.inputRoutes = !compatibility && this.options.grant.inputRouteSupported;
    this.probes = this.inputRoutes ? new TerminalPeerProbeState(this.workerFp, this.workerEpoch) : null;
    for (const sessionId of ready.sessionIds) this.readySessions.add(sessionId);
    this.tokenValue = {
      socketGeneration: generation,
      socketId: compatibility ? this.connectionId : currentSocketId,
      processEpoch: compatibility ? this.connectionId : currentEpoch,
      domainGeneration: ready.socketGeneration,
      transportKind: "loopback",
      workerFp: this.workerFp,
    };
    diag("local_terminal.ready", { worker_fp: this.workerFp, sessions: ready.sessionIds.length, socket_generation: String(ready.socketGeneration) });
    this.options.onReady(this);
  }
  private sendFrame(frame: ClientFrame): boolean {
    const socket = this.socket;
    if (!socket || socket.readyState !== 1) return false;
    try {
      socket.send(toBinary(LocalTerminalClientFrameSchema, create(LocalTerminalClientFrameSchema, { frame })));
      return true;
    } catch (error) {
      diag("local_terminal.send_failed", { case: frame.case ?? null, error: String(error) });
      return false;
    }
  }
  private waitForResponse<T>(
    waiters: Map<string, Deferred<T>>, requestId: string, timeoutMs: number, timeoutMessage: string, send: () => boolean,
  ): Promise<T> {
    if (waiters.has(requestId)) return Promise.reject(new Error("local terminal request ID is already pending"));
    const promise = new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => { waiters.delete(requestId); reject(new Error(timeoutMessage)); }, timeoutMs);
      waiters.set(requestId, { resolve, reject, timer });
    });
    if (!send()) this.rejectWaiter(waiters, requestId, "local terminal did not accept the request");
    return promise;
  }
  private resolveRouteWaiter(result: TerminalInputRouteResult): void {
    if (this.inputRoutes && result.workerEpoch === this.workerEpoch) this.resolveWaiter(this.routeWaiters, result.requestId, result);
  }
  private resolveProbeWaiter(result: TerminalTransportProbeResult): void {
    this.probes?.resolve(result);
  }
  private resolveWaiter<T>(waiters: Map<string, Deferred<T>>, requestId: string, value: T): void {
    const waiter = waiters.get(requestId);
    if (!waiter) return;
    waiters.delete(requestId);
    clearTimeout(waiter.timer);
    waiter.resolve(value);
  }
  private rejectWaiter<T>(waiters: Map<string, Deferred<T>>, requestId: string, reason: string): void {
    const waiter = waiters.get(requestId);
    if (!waiter) return;
    waiters.delete(requestId);
    clearTimeout(waiter.timer);
    waiter.reject(new Error(reason));
  }
  private finish(reason: string): void {
    if (this.ended) return;
    this.ended = true;
    this.probes?.close(reason);
    this.probes = null;
    for (const requestId of this.scrollbackWaiters.keys()) this.rejectWaiter(this.scrollbackWaiters, requestId, reason);
    for (const requestId of this.routeWaiters.keys()) this.rejectWaiter(this.routeWaiters, requestId, reason);
    if (this.tokenValue) retireTerminalInput(this.tokenValue, reason);
    this.options.onClosed(this, reason);
  }
}
class LoopbackTerminalManager {
  private connection: LoopbackTerminalConnection | null = null;
  private unregister: (() => void) | null = null;
  private grant: LocalTerminalGrant | null = null;
  private redialTimer: Timer | null = null;
  private dialAttempt = 0;
  private readonly demandedSessions = new Set<string>();
  private readonly unsubscribeGrant: () => void;
  constructor(readonly door: LocalWorkerDoor) {
    this.unsubscribeGrant = subscribeTerminalGrant(door.workerFingerprint, (grant) => this.presentGrant(grant));
    this.presentGrant(currentTerminalGrant(door.workerFingerprint));
  }
  setDemand(sessionId: string, active: boolean): void {
    if (active) this.demandedSessions.add(sessionId);
    else this.demandedSessions.delete(sessionId);
    setTerminalGrantDemand(this.door.workerFingerprint, sessionId, active);
    if (active && this.grant && !this.connection) this.dial(this.grant);
  }
  dispose(reason: string): void {
    if (this.redialTimer !== null) { clearTimeout(this.redialTimer); this.redialTimer = null; }
    this.unsubscribeGrant();
    this.unregister?.();
    this.unregister = null;
    this.connection?.close(reason);
    this.connection = null;
  }
  current(): LoopbackTerminalConnection | null { return this.connection; }
  private presentGrant(grant: LocalTerminalGrant | null): void {
    this.grant = grant;
    if (!grant) { this.connection?.close("local terminal grant cleared"); return; }
    const connection = this.connection;
    if (!connection) return this.dial(grant);
    if (connection.updateGrant(grant)) return;
    connection.close("local terminal grant scope changed");
  }
  private dial(grant: LocalTerminalGrant): void {
    if (this.connection || this.demandedSessions.size === 0) return;
    const connection = new LoopbackTerminalConnection({
      door: this.door,
      grant,
      onReady: (ready) => {
        if (this.connection !== ready) return;
        this.dialAttempt = 0;
        this.unregister = terminalDirectRegistry.register(ready);
        stageTerminalDirectConnection(ready);
      },
      onClosed: (closed, reason) => this.handleClosed(closed, reason),
      onGrantRejected: () => dropTerminalGrant(this.door.workerFingerprint),
    });
    this.connection = connection;
    connection.start();
  }
  private handleClosed(connection: LoopbackTerminalConnection, reason: string): void {
    if (this.connection !== connection) return;
    retireTerminalDirectConnection(connection, reason);
    this.unregister?.();
    this.unregister = null;
    this.connection = null;
    if (!this.grant || this.demandedSessions.size === 0) return;
    const delay = backoffDelayMs(this.dialAttempt++, { baseMs: REDIAL_BASE_MS, maxMs: REDIAL_MAX_MS });
    this.redialTimer = setTimeout(() => { this.redialTimer = null; this.dial(this.grant!); }, delay);
  }
}
let installed = false;
let manager: LoopbackTerminalManager | null = null;
const demandedSessionsByWorker = new Map<string, Set<string>>();
let unsubscribeRegistry: (() => void) | null = null;
/** Starts discovery only after a visible terminal demand and never alters Sync startup. */
export function startLocalTerminalFastPath(): void {
  if (installed) return;
  installed = true;
  unsubscribeRegistry = terminalDirectRegistry.subscribe(handleRegistryEvent);
  startTerminalPeerFastPath();
  registerSyncV2GenerationHandler((state) => {
    const door = readLocalWorkerDoor();
    if (!state?.ready || !door) return;
    clearTerminalGrantRetry(door.workerFingerprint);
    void refreshTerminalGrant(door.workerFingerprint, "sync_connected");
  });
  registerLocalWorkerDoorHandler((door) => {
    const active = demandedSessionsByWorker.get(door.workerFingerprint);
    if (!active) return;
    manager?.dispose("local worker door changed");
    manager = new LoopbackTerminalManager(door);
    for (const sessionId of active) manager.setDemand(sessionId, true);
  });
}
export function resetLocalTerminalState(reason: string): void {
  resetTerminalPeerState(reason);
  startTerminalPeerFastPath();
  resetTerminalGrants();
  manager?.dispose(reason);
  manager = null;
  demandedSessionsByWorker.clear();
}
export function _resetLocalTerminalForTest(): void {
  resetLocalTerminalState("test reset");
  resetTerminalPeerState("test reset");
  _releaseTerminalGrantRenewalForTest();
  unsubscribeRegistry?.();
  unsubscribeRegistry = null;
  installed = false;
}
function handleRegistryEvent(event: TerminalDirectRegistryEvent): void {
  if (event.kind !== "demand_changed") return;
  const active = terminalDirectRegistry.hasViewDemand(event.workerFp, event.sessionId);
  if (!recordDemand(event, active)) return;
  const door = readLocalWorkerDoor();
  if (!door || door.workerFingerprint !== event.workerFp) {
    if (active) discoverLocalWorkerDoor();
    return;
  }
  if (!manager || manager.door.workerFingerprint !== door.workerFingerprint) manager = new LoopbackTerminalManager(door);
  manager.setDemand(event.sessionId, active);
}
function recordDemand(event: Extract<TerminalDirectRegistryEvent, { kind: "demand_changed" }>, active: boolean): boolean {
  const sessions = demandedSessionsByWorker.get(event.workerFp);
  const hadDemand = sessions?.has(event.sessionId) ?? false;
  if (active === hadDemand) return false;
  if (active) {
    const nextSessions = sessions ?? new Set<string>();
    nextSessions.add(event.sessionId);
    demandedSessionsByWorker.set(event.workerFp, nextSessions);
  } else {
    sessions?.delete(event.sessionId);
    if (sessions?.size === 0) demandedSessionsByWorker.delete(event.workerFp);
  }
  return true;
}
