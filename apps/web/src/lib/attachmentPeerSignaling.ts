// WebRTC offer/answer setup for the attachment-specific peer connection.
// attachmentPeer owns authenticated data channels; this module owns SDP and ICE deadlines.
// It never shares a connection, queue, or terminal transport state.

import {
  ATTACHMENT_TRANSFER_PEER_ICE_GATHERING_DEADLINE_MS,
  ATTACHMENT_TRANSFER_PEER_NATIVE_ANSWER_DEADLINE_MS,
  ATTACHMENT_TRANSFER_PEER_NEGOTIATION_DEADLINE_MS,
} from "@roost/shared/attachment-transfer";
import {
  filterBrowserTerminalPeerUdpCandidates,
  inspectTerminalPeerSdp,
} from "@roost/shared/terminal-peer-sdp";
import { coordClient } from "../connect.ts";
import type { AttachmentDirectGrant } from "./attachmentDirectGrant.ts";

export interface AttachmentPeerSignalingDependencies {
  readonly createPeerConnection?: (configuration: RTCConfiguration) => RTCPeerConnection;
  readonly negotiate?: (
    request: { workerFp: string; grantId: string; tabId: string; peerId: string; offerSdp: string; workerEpoch: string },
    signal: AbortSignal,
  ) => Promise<{ peerId: string; answerSdp: string; workerEpoch: string }>;
}

interface Deferred<T> {
  readonly promise: Promise<T>;
  readonly resolve: (value: T) => void;
  readonly reject: (error: Error) => void;
}

/** Owns one attachment peer's candidate gathering and coordinator signaling. */
export class AttachmentPeerSignaling {
  readonly peerConnection: RTCPeerConnection;
  private gatheringTimer: ReturnType<typeof setTimeout> | null = null;
  private gatheringDeferred: Deferred<string> | null = null;
  private closed = false;

  constructor(
    stunUrls: readonly string[],
    private readonly dependencies: AttachmentPeerSignalingDependencies,
  ) {
    const createPeerConnection = dependencies.createPeerConnection ?? createBrowserPeerConnection;
    this.peerConnection = createPeerConnection({
      iceServers: stunUrls.map((url) => ({ urls: url })),
      iceTransportPolicy: "all",
    });
    this.peerConnection.onicegatheringstatechange = () => {
      if (this.peerConnection.iceGatheringState === "complete") this.resolveGathering();
    };
  }


  async negotiate(grant: AttachmentDirectGrant, peerId: string): Promise<void> {
    const controller = new AbortController();
    const offerSdp = await deadline(this.createOffer(), ATTACHMENT_TRANSFER_PEER_NEGOTIATION_DEADLINE_MS, controller);
    const response = await deadline(
      this.requestAnswer(grant, peerId, offerSdp, controller.signal),
      ATTACHMENT_TRANSFER_PEER_NATIVE_ANSWER_DEADLINE_MS,
      controller,
    );
    if (response.peerId !== peerId || response.workerEpoch !== grant.workerEpoch || !response.answerSdp) {
      throw new Error("attachment peer negotiation response was invalid");
    }
    inspectTerminalPeerSdp(response.answerSdp);
    await deadline(
      this.peerConnection.setRemoteDescription({ type: "answer", sdp: response.answerSdp }),
      ATTACHMENT_TRANSFER_PEER_NATIVE_ANSWER_DEADLINE_MS,
      controller,
    );
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    clearTimeout(this.gatheringTimer ?? undefined);
    this.gatheringTimer = null;
    const gathering = this.gatheringDeferred;
    this.gatheringDeferred = null;
    gathering?.reject(new Error("attachment peer closed"));
    this.peerConnection.onicegatheringstatechange = null;
    try { this.peerConnection.close(); } catch { /* already unusable */ }
  }

  private async createOffer(): Promise<string> {
    await this.peerConnection.setLocalDescription(await this.peerConnection.createOffer());
    const offerSdp = filterBrowserTerminalPeerUdpCandidates(await this.waitForGathering());
    if (inspectTerminalPeerSdp(offerSdp).candidateCount === 0) {
      throw new Error("attachment peer offer has no usable candidates");
    }
    return offerSdp;
  }

  private requestAnswer(
    grant: AttachmentDirectGrant,
    peerId: string,
    offerSdp: string,
    signal: AbortSignal,
  ): Promise<{ peerId: string; answerSdp: string; workerEpoch: string }> {
    const request = {
      workerFp: grant.workerFp,
      grantId: grant.grantId,
      tabId: grant.tabId,
      peerId,
      offerSdp,
      workerEpoch: grant.workerEpoch,
    };
    return this.dependencies.negotiate?.(request, signal)
      ?? coordClient.sessionsNegotiateAttachmentPeer(request, { signal });
  }

  private waitForGathering(): Promise<string> {
    if (this.peerConnection.iceGatheringState === "complete") return Promise.resolve(this.localSdp());
    const gathering = deferred<string>();
    this.gatheringDeferred = gathering;
    this.gatheringTimer = setTimeout(
      () => this.resolveGathering(),
      ATTACHMENT_TRANSFER_PEER_ICE_GATHERING_DEADLINE_MS,
    );
    return gathering.promise;
  }

  private resolveGathering(): void {
    const gathering = this.gatheringDeferred;
    if (!gathering || this.closed) return;
    this.gatheringDeferred = null;
    clearTimeout(this.gatheringTimer ?? undefined);
    this.gatheringTimer = null;
    try {
      gathering.resolve(this.localSdp());
    } catch {
      gathering.reject(new Error("attachment peer gathering failed"));
    }
  }

  private localSdp(): string {
    const sdp = this.peerConnection.localDescription?.sdp;
    if (!sdp) throw new Error("attachment peer did not produce local SDP");
    return sdp;
  }
}

function createBrowserPeerConnection(configuration: RTCConfiguration): RTCPeerConnection {
  if (typeof RTCPeerConnection === "undefined") throw new Error("WebRTC is unavailable");
  return new RTCPeerConnection(configuration);
}

async function deadline<T>(promise: Promise<T>, timeoutMs: number, controller: AbortController): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  const timeout = new Promise<never>((_resolve, reject) => {
    timer = setTimeout(() => {
      controller.abort();
      reject(new Error("attachment peer deadline elapsed"));
    }, timeoutMs);
  });
  try {
    return await Promise.race([promise, timeout]);
  } finally {
    clearTimeout(timer ?? undefined);
  }
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}
