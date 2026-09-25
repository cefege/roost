// Native node-datachannel answerer for one authenticated attachment peer.
// It creates only attachment channels and packet port, verifies the browser DTLS
// fingerprint, and leaves terminal PeerConnections and their lifecycle untouched.

import { randomUUID } from "node:crypto";
import {
  ATTACHMENT_TRANSFER_PACKET_MAX_BYTES,
  ATTACHMENT_TRANSFER_PEER_DATA_CHANNELS,
  ATTACHMENT_TRANSFER_PEER_ICE_GATHERING_DEADLINE_MS,
  type AttachmentTransferPeerChannelLane,
} from "@roost/protocol/attachment-transfer";
import {
  inspectTerminalPeerSdp,
  normalizeTerminalPeerSha256Fingerprint,
} from "@roost/protocol/terminal-peer-sdp";
import type { TerminalPeerNative } from "../terminal/peer/terminal-peer-native.ts";
import type { AttachmentPeerPacketPeerBudget } from "./attachment-peer-packet-budget.ts";
import {
  AttachmentPeerPacketPort,
  type AttachmentPeerNativeDataChannel,
  type AttachmentPeerNativeDataChannels,
  type AttachmentPeerPacketIngress,
} from "./attachment-peer-packet-port.ts";

export type AttachmentPeerConnectionFailureReason = "ice_failed" | "connection_superseded";

export class AttachmentPeerConnectionError extends Error {
  constructor(readonly reason: AttachmentPeerConnectionFailureReason) {
    super(`attachment peer connection failed: ${reason}`);
    this.name = "AttachmentPeerConnectionError";
  }
}

export interface AttachmentPeerExpectedTuple {
  readonly peerId: string;
  readonly grantId: string;
  readonly deviceFingerprint: string;
  readonly tabId: string;
  readonly workerEpoch: string;
}

export interface AttachmentPeerConnectionConfig {
  readonly stunUrls: readonly string[];
  readonly bindAddress?: string;
  readonly portRange?: { readonly min: number; readonly max: number };
}

export type OpenAttachmentPeerPort = (
  port: AttachmentPeerPacketPort,
  expectedTuple: AttachmentPeerExpectedTuple,
) => AttachmentPeerPacketIngress;

export interface AttachmentPeerConnectionDeps {
  readonly native: TerminalPeerNative;
  readonly peerId: string;
  readonly expectedTuple: AttachmentPeerExpectedTuple;
  readonly expectedRemoteFingerprint: string;
  readonly config: AttachmentPeerConnectionConfig;
  readonly packetBudget: AttachmentPeerPacketPeerBudget;
  readonly openPeerPort: OpenAttachmentPeerPort;
  readonly onClosed: (reason: AttachmentPeerConnectionFailureReason) => void;
  readonly socketId?: string;
}

interface AnswerWaiter {
  resolve(answerSdp: string): void;
  reject(error: AttachmentPeerConnectionError): void;
}

/** One immutable attachment offer/answer exchange. */
export class AttachmentPeerConnection {
  readonly port!: AttachmentPeerPacketPort;
  private readonly peer: InstanceType<TerminalPeerNative["PeerConnection"]>;
  private answerWaiter: AnswerWaiter | undefined;
  private gatheringTimer: NodeJS.Timeout | undefined;
  private fingerprintVerified = false;
  private closed = false;
  private closedNotified = false;

  constructor(private readonly deps: AttachmentPeerConnectionDeps) {
    this.peer = new deps.native.PeerConnection(`roost-attachment-peer-${deps.peerId}`, {
      iceServers: [...deps.config.stunUrls],
      disableAutoNegotiation: true,
      enableIceTcp: false,
      disableFingerprintVerification: false,
      iceTransportPolicy: "all",
      maxMessageSize: ATTACHMENT_TRANSFER_PACKET_MAX_BYTES,
      ...(deps.config.bindAddress === undefined ? {} : { bindAddress: deps.config.bindAddress }),
      ...(deps.config.portRange === undefined
        ? {}
        : {
          portRangeBegin: deps.config.portRange.min,
          portRangeEnd: deps.config.portRange.max,
        }),
    });
    this.installPeerCallbacks();
    let packetPort: AttachmentPeerPacketPort | undefined;
    try {
      packetPort = new AttachmentPeerPacketPort({
        socketId: deps.socketId ?? randomUUID(),
        channels: this.createDataChannels(),
        packetBudget: deps.packetBudget,
        onClosed: () => { this.close("connection_superseded"); },
      });
      this.port = packetPort;
      const ingress = deps.openPeerPort(packetPort, deps.expectedTuple);
      if (!ingress || typeof ingress.onMessage !== "function") {
        throw new Error("attachment peer ingress is unavailable");
      }
      packetPort.attachIngress(ingress);
    } catch (error) {
      const reason = error instanceof AttachmentPeerConnectionError ? error.reason : "connection_superseded";
      packetPort?.close(undefined, reason);
      deps.packetBudget.dispose();
      try { this.peer.close(); } catch { /* constructor failure has no live native peer */ }
      throw new AttachmentPeerConnectionError(reason);
    }
  }

  get isClosed(): boolean {
    return this.closed;
  }

  answer(offerSdp: string, deadlineMs: number): Promise<string> {
    if (this.closed || this.answerWaiter !== undefined) {
      return Promise.reject(new AttachmentPeerConnectionError("connection_superseded"));
    }
    const answer = new Promise<string>((resolve, reject) => { this.answerWaiter = { resolve, reject }; });
    try {
      this.peer.setRemoteDescription(offerSdp, "offer");
      this.peer.setLocalDescription("answer");
    } catch {
      this.fail("ice_failed");
      return answer;
    }
    const gatheringDeadlineMs = Math.min(ATTACHMENT_TRANSFER_PEER_ICE_GATHERING_DEADLINE_MS, deadlineMs);
    if (!Number.isFinite(gatheringDeadlineMs) || gatheringDeadlineMs <= 0) {
      this.fail("ice_failed");
      return answer;
    }
    this.gatheringTimer = setTimeout(() => {
      this.gatheringTimer = undefined;
      const partialAnswer = this.answerWithCandidates();
      if (partialAnswer === null) this.fail("ice_failed");
      else this.finishAnswer(partialAnswer);
    }, gatheringDeadlineMs);
    this.gatheringTimer.unref?.();
    this.resolveCompleteAnswer();
    return answer;
  }

  close(reason: AttachmentPeerConnectionFailureReason = "connection_superseded"): void {
    if (this.closed) return;
    this.closed = true;
    clearTimeout(this.gatheringTimer);
    this.gatheringTimer = undefined;
    const waiter = this.answerWaiter;
    this.answerWaiter = undefined;
    waiter?.reject(new AttachmentPeerConnectionError(reason));
    this.port.close(undefined, reason);
    try { this.peer.close(); } catch { /* native peer is already terminal */ }
    this.notifyClosed(reason);
  }

  private installPeerCallbacks(): void {
    this.peer.onLocalDescription(() => { this.resolveCompleteAnswer(); });
    this.peer.onGatheringStateChange(() => { this.resolveCompleteAnswer(); });
    this.peer.onStateChange((state) => {
      if (state === "connected") this.verifyRemoteFingerprint();
      else if (state === "failed" || state === "closed") this.fail("ice_failed");
    });
    this.peer.onIceStateChange((state) => {
      if (state === "failed") this.fail("ice_failed");
    });
    this.peer.onDataChannel(() => { this.fail("ice_failed"); });
    this.peer.onTrack(() => { this.fail("ice_failed"); });
  }

  private createDataChannels(): AttachmentPeerNativeDataChannels {
    const channels: Partial<AttachmentPeerNativeDataChannels> = {};
    for (const definition of ATTACHMENT_TRANSFER_PEER_DATA_CHANNELS) {
      channels[definition.lane] = this.peer.createDataChannel(definition.label, {
        negotiated: true,
        id: definition.id,
        unordered: !definition.ordered,
        protocol: definition.protocol,
      }) as unknown as AttachmentPeerNativeDataChannel;
    }
    if (!channels.control || !channels.data) throw new AttachmentPeerConnectionError("ice_failed");
    return channels as Record<AttachmentTransferPeerChannelLane, AttachmentPeerNativeDataChannel>;
  }

  private resolveCompleteAnswer(): void {
    if (this.closed || this.answerWaiter === undefined || this.peer.gatheringState() !== "complete") return;
    const answerSdp = this.answerWithCandidates();
    if (answerSdp !== null) this.finishAnswer(answerSdp);
  }

  private answerWithCandidates(): string | null {
    const description = this.peer.localDescription();
    if (!description || description.type !== "answer") return null;
    try {
      return inspectTerminalPeerSdp(description.sdp).candidateCount > 0 ? description.sdp : null;
    } catch {
      return null;
    }
  }

  private finishAnswer(answerSdp: string): void {
    const waiter = this.answerWaiter;
    if (!waiter) return;
    this.answerWaiter = undefined;
    clearTimeout(this.gatheringTimer);
    this.gatheringTimer = undefined;
    waiter.resolve(answerSdp);
  }

  private verifyRemoteFingerprint(): void {
    if (this.closed || this.fingerprintVerified) return;
    try {
      const fingerprint = this.peer.remoteFingerprint();
      if (
        fingerprint.algorithm.toLowerCase() !== "sha-256"
        || normalizeTerminalPeerSha256Fingerprint(fingerprint.value) !== this.deps.expectedRemoteFingerprint
      ) {
        this.fail("ice_failed");
        return;
      }
      this.fingerprintVerified = true;
    } catch {
      this.fail("ice_failed");
    }
  }

  private fail(reason: AttachmentPeerConnectionFailureReason): void {
    this.close(reason);
  }

  private notifyClosed(reason: AttachmentPeerConnectionFailureReason): void {
    if (this.closedNotified) return;
    this.closedNotified = true;
    this.deps.onClosed(reason);
  }
}
