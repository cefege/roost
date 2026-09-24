// Browser WebRTC adapter for one authenticated worker terminal peer.
// It owns static channels, shared packet reassembly and direct-carrier request
// correlation. The peer composer supplies lifecycle/input callbacks; this file
// imports no worker-native runtime or coordinator implementation.
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
import {
  TERMINAL_PEER_CHANNEL_WATERMARKS,
  TERMINAL_PEER_DATA_CHANNELS,
  TERMINAL_PEER_HELLO_DEADLINE_MS,
  TERMINAL_PEER_ICE_GATHERING_DEADLINE_MS,
  TERMINAL_PEER_LANE_PRIORITY,
  TERMINAL_PEER_NEGOTIATION_DEADLINE_MS,
  TERMINAL_PEER_MAX_FLUSH_BYTES_PER_TURN,
  type TerminalPeerPacketLane,
} from "@roost/protocol/terminal-peer";
import {
  TerminalPeerPacketAssembler,
  TerminalPeerPacketError,
  TerminalPeerPacketQueue,
  parseTerminalPeerPacket,
} from "@roost/protocol/terminal-peer-packets";
import { filterBrowserTerminalPeerUdpCandidates, inspectTerminalPeerSdp } from "@roost/protocol/terminal-peer-sdp";
import { dispatchDirectTerminalFrame } from "../terminal-stream-promotion.ts";
import type { LocalScrollbackQuery, TerminalDirectConnection } from "../terminal-stream-transport.ts";
import type { TerminalGenerationToken } from "../terminal-stream-types.ts";
import type { LocalTerminalGrant } from "./local-terminal-grants.ts";
import { retireTerminalInput } from "./terminal-input-router.ts";
import { createTerminalDirectRequestId, terminalDirectBufferSource } from "../../client/carriers/terminal-direct-browser.ts";
import { BrowserTerminalPeerPacketBudget } from "../../client/carriers/terminal-peer-packet-budget.ts";
import { TerminalPeerPacketStalls } from "../../client/carriers/terminal-peer-packet-stalls.ts";
import { TerminalPeerTelemetry } from "./terminal-peer-telemetry.ts";
import { TerminalPeerProbeState } from "../../client/carriers/terminal-peer-probe-state.ts";
const SCROLLBACK_TIMEOUT_MS = 15_000, ROUTE_CLAIM_TIMEOUT_MS = 8_000;
type ClientFrame = LocalTerminalClientFrame["frame"];
type InputResultFrame = Extract<LocalTerminalServerFrame["frame"], {
  case: "inputAccepted" | "inputRejected" | "inputAmbiguous";
}>;
interface Deferred<T> { readonly resolve: (value: T) => void; readonly reject: (reason: Error) => void; readonly timer: Timer; }
export interface TerminalPeerConnectionHooks { readonly onReady: (connection: TerminalPeerConnection) => void; readonly onClosed: (connection: TerminalPeerConnection, reason: string) => void; readonly onInputResult: (token: TerminalGenerationToken, frame: InputResultFrame) => void; }
export interface TerminalPeerConnectionOptions { readonly workerFp: string; readonly workerEpoch: string; readonly peerId: string; readonly grant: LocalTerminalGrant; readonly stunUrls: readonly string[]; readonly hooks: TerminalPeerConnectionHooks; }
export interface TerminalPeerConnectionDependencies { readonly createPeerConnection?: (configuration: RTCConfiguration) => RTCPeerConnection; }
/** One immutable direct generation. Construct a replacement after every close. */
export class TerminalPeerConnection implements TerminalDirectConnection {
  readonly kind = "webrtc" as const;
  readonly connectionId = createTerminalDirectRequestId();
  readonly workerFp: string;
  readonly workerEpoch: string;
  private readonly peerConnection: RTCPeerConnection;
  private readonly channels = new Map<TerminalPeerPacketLane, RTCDataChannel>();
  private readonly outboundBudget = new BrowserTerminalPeerPacketBudget();
  private readonly inboundBudget = new BrowserTerminalPeerPacketBudget();
  private readonly outbound = {} as Record<TerminalPeerPacketLane, TerminalPeerPacketQueue>;
  private readonly inbound = {} as Record<TerminalPeerPacketLane, TerminalPeerPacketAssembler>;
  private readonly readySessions = new Set<string>();
  private readonly scrollbackWaiters = new Map<string, Deferred<LocalScrollbackResponse>>();
  private readonly routeWaiters = new Map<string, Deferred<TerminalInputRouteResult>>();
  private tokenValue: TerminalGenerationToken | null = null;
  private helloTimer: Timer | null = null;
  private gatheringTimer: Timer | null = null;
  private readonly stalls: TerminalPeerPacketStalls;
  private setupTimer: Timer | null = null;
  private gatheringResolve: ((sdp: string) => void) | null = null;
  private gatheringReject: ((error: Error) => void) | null = null;
  private flushScheduled = false;
  private closed = false;
  private authenticated = false;
  private inputRoutes = false;
  private readonly probes: TerminalPeerProbeState;
  private readonly peerTelemetry = new TerminalPeerTelemetry();

  private constructor(private readonly options: TerminalPeerConnectionOptions, deps: TerminalPeerConnectionDependencies) {
    this.workerFp = options.workerFp;
    this.workerEpoch = options.workerEpoch;
    this.inputRoutes = options.grant.inputRouteSupported;
    this.probes = new TerminalPeerProbeState(options.workerFp, options.workerEpoch);
    const createPeerConnection = deps.createPeerConnection ?? createBrowserPeerConnection;
    this.peerConnection = createPeerConnection({ iceServers: options.stunUrls.map((url) => ({ urls: url })), iceTransportPolicy: "all" });
    for (const lane of TERMINAL_PEER_LANE_PRIORITY) {
      this.outbound[lane] = new TerminalPeerPacketQueue(lane, this.outboundBudget.quota(lane));
      this.inbound[lane] = new TerminalPeerPacketAssembler(lane, this.inboundBudget.quota(lane));
    }
    this.stalls = new TerminalPeerPacketStalls(
      (lane) => this.inbound[lane].expire(),
      () => this.close("terminal peer fragment stalled"),
    );
    try { this.installPeerHandlers(); this.createStaticChannels(); }
    catch (error) { try { this.peerConnection.close(); } catch { /* construction is already failing */ } this.stalls.close(); throw error; }
    this.setupTimer = setTimeout(
      () => this.close("terminal peer setup timed out"),
      TERMINAL_PEER_NEGOTIATION_DEADLINE_MS,
    );
  }

  static create(options: TerminalPeerConnectionOptions, deps: TerminalPeerConnectionDependencies = {}): TerminalPeerConnection {
    return new TerminalPeerConnection(options, deps);
  }
  token(): TerminalGenerationToken | null { return this.tokenValue; }
  allowsSession(sessionId: string): boolean { return this.authenticated && this.readySessions.has(sessionId); }
  get inputRouteSupported(): boolean { return this.inputRoutes; }
  telemetry() {
    return {
      opaquePeerId: this.options.peerId,
      ...this.probes.telemetry(),
      candidateType: this.peerTelemetry.candidateType(this.peerConnection),
      bufferedBytes: TERMINAL_PEER_LANE_PRIORITY.reduce(
        (total, lane) => total + this.outbound[lane].queuedBytes + (this.channels.get(lane)?.bufferedAmount ?? 0),
        0,
      ),
    };
  }
  requireFreshProbe(): void { this.probes.requireFresh(); }
  updateGrant(grant: LocalTerminalGrant): boolean {
    if (!grant.peerSupported || grant.workerFp !== this.workerFp || grant.workerEpoch !== this.workerEpoch) return false;
    if (!this.authenticated && grant.sessionIds.some((sessionId) => !this.options.grant.sessionIds.includes(sessionId))) return false;
    if ([...this.readySessions].some((sessionId) => !grant.sessionIds.includes(sessionId))) return false;
    for (const sessionId of grant.sessionIds) this.readySessions.add(sessionId);
    this.inputRoutes = grant.inputRouteSupported; return true;
  }
  async createOffer(): Promise<string> {
    this.assertOpen();
    await this.peerConnection.setLocalDescription(await this.peerConnection.createOffer());
    const sdp = filterBrowserTerminalPeerUdpCandidates(await this.waitForGathering());
    if (inspectTerminalPeerSdp(sdp).candidateCount === 0) throw new Error("terminal peer offer has no usable candidates");
    return sdp;
  }
  async acceptAnswer(answerSdp: string): Promise<void> {
    this.assertOpen();
    if (inspectTerminalPeerSdp(answerSdp).candidateCount === 0) throw new Error("terminal peer answer has no usable candidates");
    await this.peerConnection.setRemoteDescription({ type: "answer", sdp: answerSdp });
  }
  publishView(command: TerminalViewCommand): boolean {
    return this.allowsSession(command.sessionId) && this.sendFrame({ case: "terminalView", value: command }, "control");
  }
  publishResync(command: TerminalResyncCommand): boolean {
    return this.allowsSession(command.sessionId) && this.sendFrame({ case: "terminalResync", value: command }, "control");
  }
  sendInput(command: InputCommand): "accepted" | "refused" {
    return this.allowsSession(command.sessionId) && this.sendFrame({ case: "input", value: command }, "control") ? "accepted" : "refused";
  }
  claimInputRoute(command: TerminalInputRouteClaim): Promise<TerminalInputRouteResult> {
    if (!this.inputRouteSupported || !this.authenticated || command.workerEpoch !== this.workerEpoch) return Promise.reject(new Error("terminal peer input route is unavailable"));
    return this.waitForResponse(this.routeWaiters, command.requestId, ROUTE_CLAIM_TIMEOUT_MS, "terminal peer input route claim timed out", () =>
      this.sendFrame({ case: "inputRouteClaim", value: command }, "control"));
  }
  requestScrollback(query: LocalScrollbackQuery): Promise<LocalScrollbackResponse> {
    if (!this.allowsSession(query.sessionId)) return Promise.reject(new Error("terminal peer is not authorized for this session"));
    const requestId = createTerminalDirectRequestId();
    return this.waitForResponse(this.scrollbackWaiters, requestId, SCROLLBACK_TIMEOUT_MS, "terminal peer scrollback request timed out", () =>
      this.sendFrame({ case: "scrollback", value: create(LocalScrollbackRequestSchema, { ...query, requestId }) }, "control"));
  }
  probe(requestId: string): Promise<void> {
    if (!this.authenticated) return Promise.reject(new Error("terminal peer probe is unavailable"));
    return this.probes.start(requestId, () => this.sendFrame({
      case: "transportProbe",
      value: create(TerminalTransportProbeSchema, {
        requestId,
        workerFp: this.workerFp,
      }),
    }, "control"));
  }

  close(reason: string): void {
    if (this.closed) return;
    this.closed = true;
    diag("terminal_peer.connection_closed", { reason });
    if (this.helloTimer !== null) { clearTimeout(this.helloTimer); this.helloTimer = null; }
    if (this.gatheringTimer !== null) { clearTimeout(this.gatheringTimer); this.gatheringTimer = null; }
    if (this.setupTimer !== null) { clearTimeout(this.setupTimer); this.setupTimer = null; }
    this.stalls.close();
    this.gatheringReject?.(new Error(reason));
    this.gatheringReject = null;
    this.gatheringResolve = null;
    this.rejectWaiters(this.scrollbackWaiters, reason);
    this.rejectWaiters(this.routeWaiters, reason);
    this.probes.close(reason);
    if (this.tokenValue) retireTerminalInput(this.tokenValue, reason);
    for (const lane of TERMINAL_PEER_LANE_PRIORITY) {
      this.outbound[lane].clear();
      this.inbound[lane].reset();
      const channel = this.channels.get(lane);
      if (channel) closeDataChannel(channel);
    }
    closePeerConnection(this.peerConnection);
    this.options.hooks.onClosed(this, reason);
  }

  private installPeerHandlers(): void {
    this.peerConnection.ondatachannel = () => this.close("terminal peer offered an unsolicited data channel");
    this.peerConnection.ontrack = () => this.close("terminal peer offered a media track");
    this.peerConnection.onconnectionstatechange = () => {
      const connectionState = this.peerConnection.connectionState;
      if (connectionState === "connected") this.peerTelemetry.captureSelectedCandidateType(this.peerConnection);
      if (["failed", "closed"].includes(connectionState)) this.close(`terminal peer connection ${connectionState}`);
    };
    this.peerConnection.oniceconnectionstatechange = () => {
      if (["failed", "closed"].includes(this.peerConnection.iceConnectionState)) this.close(`terminal peer ICE ${this.peerConnection.iceConnectionState}`);
    };
    this.peerConnection.onicegatheringstatechange = () => { if (this.peerConnection.iceGatheringState === "complete") this.resolveGathering(); };
  }
  private createStaticChannels(): void {
    for (const definition of TERMINAL_PEER_DATA_CHANNELS) {
      const channel = this.peerConnection.createDataChannel(definition.label, {
        negotiated: true, id: definition.id, ordered: true, protocol: definition.protocol,
      });
      channel.binaryType = "arraybuffer";
      channel.bufferedAmountLowThreshold = TERMINAL_PEER_CHANNEL_WATERMARKS[definition.lane].lowBytes;
      channel.onopen = () => this.handleChannelOpen(definition.lane);
      channel.onclose = () => this.close(`terminal peer ${definition.lane} channel closed`);
      channel.onerror = () => this.close(`terminal peer ${definition.lane} channel failed`);
      channel.onbufferedamountlow = () => this.scheduleFlush();
      channel.onmessage = (event) => this.receivePacket(definition.lane, event.data);
      this.channels.set(definition.lane, channel);
    }
  }
  private handleChannelOpen(lane: TerminalPeerPacketLane): void {
    if (this.closed) return;
    if (lane === "control") {
      this.helloTimer = setTimeout(() => this.close("terminal peer hello timed out"), TERMINAL_PEER_HELLO_DEADLINE_MS);
      this.sendFrame({ case: "hello", value: create(LocalTerminalHelloSchema, {
        grantId: this.options.grant.grantId, secret: this.options.grant.secret, tabId: this.options.grant.tabId,
        deviceFingerprint: this.options.grant.deviceFingerprint, peerId: this.options.peerId, workerEpoch: this.workerEpoch,
      }) }, "control");
    }
    this.scheduleFlush();
  }
  private receivePacket(lane: TerminalPeerPacketLane, data: unknown): void {
    if (this.closed) return;
    const bytes = binaryBytes(data);
    if (!bytes) return this.close("terminal peer received a non-binary packet");
    try {
      if (!this.authenticated && lane !== "control") return this.close("terminal peer sent data before hello");
      const message = this.inbound[lane].push(bytes);
      this.stalls.update(lane, this.inbound[lane].hasPartialMessage);
      if (message) this.handleServerFrame(lane, fromBinary(LocalTerminalServerFrameSchema, message));
    } catch (error) {
      this.close(`terminal peer rejected ${error instanceof TerminalPeerPacketError ? error.code : "undecodable frame"}`);
    }
  }
  private handleServerFrame(lane: TerminalPeerPacketLane, frame: LocalTerminalServerFrame): void {
    const oneof = frame.frame;
    if (!this.authenticated) {
      if (lane !== "control" || oneof.case !== "ready") return this.close("terminal peer required Ready as its first control frame");
      return this.admitReady(oneof.value);
    }
    if (!allowsServerFrame(lane, oneof.case)) return this.close("terminal peer used an invalid channel lane");
    const token = this.tokenValue;
    if (!token) return this.close("terminal peer lost its generation token");
    if (oneof.case === "inputAccepted" || oneof.case === "inputRejected" || oneof.case === "inputAmbiguous") return this.options.hooks.onInputResult(token, oneof);
    if (oneof.case === "inputRouteResult") return this.resolveRouteWaiter(oneof.value);
    if (oneof.case === "transportProbeResult") return this.resolveProbeWaiter(oneof.value);
    if (oneof.case === "scrollback") return this.resolveWaiter(this.scrollbackWaiters, oneof.value.requestId, oneof.value);
    if (oneof.case === "closed") return this.close(oneof.value.reason || "terminal peer closed");
    dispatchDirectTerminalFrame(token, frame);
  }
  private admitReady(ready: Extract<LocalTerminalServerFrame["frame"], { case: "ready" }>["value"]): void {
    const generation = Number(ready.socketGeneration);
    if (
      ready.workerFingerprint !== this.workerFp || ready.workerEpoch !== this.workerEpoch || ready.peerId !== this.options.peerId
      || !ready.socketId || !Number.isSafeInteger(generation)
      || ready.sessionIds.some((sessionId) => !this.options.grant.sessionIds.includes(sessionId))
    ) return this.close("terminal peer Ready did not match its authenticated tuple");
    if (this.setupTimer !== null) { clearTimeout(this.setupTimer); this.setupTimer = null; }
    this.authenticated = true;
    if (this.helloTimer !== null) { clearTimeout(this.helloTimer); this.helloTimer = null; }
    for (const sessionId of ready.sessionIds) this.readySessions.add(sessionId);
    this.tokenValue = { socketGeneration: generation, socketId: ready.socketId, processEpoch: ready.workerEpoch,
      domainGeneration: ready.socketGeneration, transportKind: "webrtc", workerFp: this.workerFp };
    this.peerTelemetry.captureSelectedCandidateType(this.peerConnection);
    this.options.hooks.onReady(this);
  }
  private sendFrame(frame: ClientFrame, lane: TerminalPeerPacketLane): boolean {
    if (this.closed || this.channels.get(lane)?.readyState !== "open") return false;
    try {
      if (!this.outbound[lane].enqueue(toBinary(LocalTerminalClientFrameSchema, create(LocalTerminalClientFrameSchema, { frame })))) return false;
      this.scheduleFlush();
      return true;
    } catch {
      this.close("terminal peer could not queue a client frame");
      return false;
    }
  }
  private scheduleFlush(): void {
    if (this.closed || this.flushScheduled) return;
    this.flushScheduled = true;
    queueMicrotask(() => this.flush());
  }
  private flush(): void {
    this.flushScheduled = false;
    if (this.closed) return;
    let sentBytes = 0;
    let sentAny = false;
    for (const lane of TERMINAL_PEER_LANE_PRIORITY) {
      const channel = this.channels.get(lane);
      if (!channel || channel.readyState !== "open") continue;
      while (sentBytes < TERMINAL_PEER_MAX_FLUSH_BYTES_PER_TURN && channel.bufferedAmount <= TERMINAL_PEER_CHANNEL_WATERMARKS[lane].highBytes) {
        const fragment = this.outbound[lane].nextFragment();
        if (!fragment) break;
        try {
          channel.send(terminalDirectBufferSource(fragment.bytes));
          fragment.commit();
          sentBytes += fragment.bytes.byteLength;
          sentAny = true;
        } catch {
          this.close("terminal peer data channel send failed");
          return;
        }
      }
    }
    if (sentAny && TERMINAL_PEER_LANE_PRIORITY.some((lane) => this.outbound[lane].messageCount > 0)) this.scheduleFlush();
  }
  private waitForGathering(): Promise<string> {
    if (this.peerConnection.iceGatheringState === "complete") return Promise.resolve(this.gatheredSdp());
    return new Promise<string>((resolve, reject) => {
      this.gatheringResolve = resolve;
      this.gatheringReject = reject;
      this.gatheringTimer = setTimeout(() => this.resolveGathering(), TERMINAL_PEER_ICE_GATHERING_DEADLINE_MS);
    });
  }
  private resolveGathering(): void {
    const resolve = this.gatheringResolve, reject = this.gatheringReject;
    if (!resolve || this.closed) return;
    if (this.gatheringTimer !== null) { clearTimeout(this.gatheringTimer); this.gatheringTimer = null; }
    this.gatheringResolve = null; this.gatheringReject = null;
    try { resolve(this.gatheredSdp()); }
    catch (error) { const failure = error instanceof Error ? error : new Error("terminal peer gathering failed"); reject?.(failure); this.close(failure.message); }
  }
  private gatheredSdp(): string {
    const sdp = this.peerConnection.localDescription?.sdp;
    if (!sdp) throw new Error("terminal peer did not produce local SDP");
    return sdp;
  }
  private waitForResponse<T>(
    waiters: Map<string, Deferred<T>>, requestId: string, timeoutMs: number, timeoutMessage: string, send: () => boolean,
  ): Promise<T> {
    if (waiters.has(requestId)) return Promise.reject(new Error("terminal peer request ID is already pending"));
    const promise = new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => { waiters.delete(requestId); reject(new Error(timeoutMessage)); }, timeoutMs);
      waiters.set(requestId, { resolve, reject, timer });
    });
    if (!send()) this.rejectWaiter(waiters, requestId, "terminal peer did not accept the request");
    return promise;
  }
  private resolveRouteWaiter(result: TerminalInputRouteResult): void {
    if (result.workerEpoch === this.workerEpoch) this.resolveWaiter(this.routeWaiters, result.requestId, result);
  }
  private resolveProbeWaiter(result: TerminalTransportProbeResult): void {
    this.probes.resolve(result);
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
  private rejectWaiters<T>(waiters: Map<string, Deferred<T>>, reason: string): void {
    for (const requestId of waiters.keys()) this.rejectWaiter(waiters, requestId, reason);
  }
  private assertOpen(): void { if (this.closed) throw new Error("terminal peer is closed"); }
}
function createBrowserPeerConnection(configuration: RTCConfiguration): RTCPeerConnection { if (typeof RTCPeerConnection === "undefined") throw new Error("WebRTC is unavailable"); return new RTCPeerConnection(configuration); }
function binaryBytes(data: unknown): Uint8Array | null {
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  if (ArrayBuffer.isView(data)) return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  return null;
}
function allowsServerFrame(lane: TerminalPeerPacketLane, frameCase: LocalTerminalServerFrame["frame"]["case"]): boolean {
  if (!frameCase) return false;
  if (lane === "control") return ["inputAccepted", "inputRejected", "inputAmbiguous", "inputRouteResult", "transportProbeResult", "closed"].includes(frameCase);
  return lane === "terminal" ? ["terminalViewState", "cellGrid", "cellGridChunk"].includes(frameCase) : frameCase === "scrollback";
}
function closeDataChannel(channel: RTCDataChannel): void { try { channel.close(); } catch { /* already closed */ } }
function closePeerConnection(peerConnection: RTCPeerConnection): void { try { peerConnection.close(); } catch { /* already closed */ } }
