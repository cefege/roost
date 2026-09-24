// Lifecycle owner for authenticated native browser attachment peers.
// It has independent admission, PC, packet budget, and close state from terminal
// peers while borrowing only the process-owned native runtime loader.

import { create } from "@bufbuild/protobuf";
import { log } from "@roost/observability/log";
import {
  ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_WORKER,
  ATTACHMENT_TRANSFER_PEER_MAX_NEGOTIATIONS_PER_WORKER,
  ATTACHMENT_TRANSFER_PEER_MAX_PENDING_NEGOTIATIONS_PER_DEVICE,
  ATTACHMENT_TRANSFER_PEER_NATIVE_ANSWER_DEADLINE_MS,
  type AttachmentTransferPeerErrorReason,
} from "@roost/protocol/attachment-transfer";
import { inspectTerminalPeerSdp } from "@roost/protocol/terminal-peer-sdp";
import { parseTerminalPeerStunUrls } from "@roost/protocol/terminal-peer";
import {
  WLocalAttachmentPeerAnswerSchema,
  type DLocalAttachmentPeerCancel,
  type DLocalAttachmentPeerOffer,
  type WLocalAttachmentPeerAnswer,
} from "@roost/protocol/proto/worker_transport_pb";
import type { AttachmentPeerGrantAuthorization } from "./attachment-grants.ts";
import {
  AttachmentPeerConnection,
  AttachmentPeerConnectionError,
  type AttachmentPeerConnectionConfig,
  type AttachmentPeerConnectionFailureReason,
  type AttachmentPeerExpectedTuple,
  type OpenAttachmentPeerPort,
} from "./attachment-peer-connection.ts";
import { loadTerminalPeerNative, type TerminalPeerNative } from "./terminal-peer-native.ts";
import { AttachmentPeerPacketBudget } from "./attachment-peer-packet-budget.ts";
import { validAttachmentPeerOfferIdentity } from "./attachment-peer-request-validation.ts";
import type { TerminalRequestBudget } from "./transport/coord-link-types.ts";

export type AttachmentPeerBootstrapState = "disabled" | "native_unavailable" | "ready";
export type AttachmentPeerOfferFailureReason = AttachmentTransferPeerErrorReason;

export class AttachmentPeerOfferError extends Error {
  constructor(readonly reason: AttachmentPeerOfferFailureReason) {
    super(`attachment peer offer failed: ${reason}`);
    this.name = "AttachmentPeerOfferError";
  }
}

export interface AttachmentPeerOwnerDeps {
  readonly processEpoch: string;
  readonly enabled: boolean;
  readonly bindAddress?: string;
  readonly portRange?: { readonly min: number; readonly max: number };
  readonly isCurrentCoordinator: (connectionGeneration: string) => boolean;
  readonly authorizeGrant: (request: DLocalAttachmentPeerOffer) => AttachmentPeerGrantAuthorization;
  readonly openPeerPort: OpenAttachmentPeerPort;
  /** The terminal peer owner remains the sole cleaner of this process-wide runtime. */
  readonly nativeLoader?: () => Promise<TerminalPeerNative>;
  readonly packetBudget?: AttachmentPeerPacketBudget;
}

interface PendingPeer {
  readonly request: DLocalAttachmentPeerOffer;
  readonly budget: TerminalRequestBudget;
  readonly expectedTuple: AttachmentPeerExpectedTuple;
  readonly config: AttachmentPeerConnectionConfig;
  cancelled: boolean;
  connection: AttachmentPeerConnection | undefined;
}

interface ActivePeer {
  readonly expectedTuple: AttachmentPeerExpectedTuple;
  readonly connection: AttachmentPeerConnection;
}

/** One worker's bounded attachment peer owner. */
export class AttachmentPeerOwner {
  private readonly packetBudget: AttachmentPeerPacketBudget;
  private readonly pending = new Map<string, PendingPeer>();
  private readonly active = new Map<string, ActivePeer>();
  private bootstrapPromise: Promise<AttachmentPeerBootstrapState> | undefined;
  private bootstrapState: "idle" | AttachmentPeerBootstrapState = "idle";
  private native: TerminalPeerNative | undefined;
  private disposed = false;

  constructor(private readonly deps: AttachmentPeerOwnerDeps) {
    this.packetBudget = deps.packetBudget ?? new AttachmentPeerPacketBudget();
  }

  get capabilityState(): AttachmentPeerBootstrapState | "idle" {
    return this.bootstrapState;
  }

  get establishedCount(): number {
    return this.active.size;
  }

  async bootstrap(): Promise<AttachmentPeerBootstrapState> {
    if (!this.deps.enabled) {
      if (this.bootstrapState === "idle") {
        this.bootstrapState = "disabled";
        log.info("attachment-peer", "native_disabled", {});
      }
      return "disabled";
    }
    if (this.disposed) return "native_unavailable";
    this.bootstrapPromise ??= this.loadNative();
    return await this.bootstrapPromise;
  }

  async offer(
    request: DLocalAttachmentPeerOffer,
    budget: TerminalRequestBudget,
  ): Promise<WLocalAttachmentPeerAnswer> {
    const admissionFailure = this.admissionFailure(request, budget);
    if (admissionFailure !== null) throw new AttachmentPeerOfferError(admissionFailure);
    let remoteFingerprint: string;
    let stunUrls: string[];
    try {
      if (request.stunUrls.length > 4 || request.stunUrls.some((url) => url.length === 0 || url.includes(","))) {
        throw new Error("invalid STUN URLs");
      }
      remoteFingerprint = inspectTerminalPeerSdp(request.offerSdp).fingerprintSha256;
      stunUrls = parseTerminalPeerStunUrls(request.stunUrls.join(","));
    } catch {
      throw new AttachmentPeerOfferError("invalid_offer");
    }
    if (
      this.pending.size >= ATTACHMENT_TRANSFER_PEER_MAX_NEGOTIATIONS_PER_WORKER
      || this.active.size + this.pending.size >= ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_WORKER
      || this.pending.has(request.requestId)
      || this.active.has(request.peerId)
      || this.hasPendingPeerId(request.peerId)
      || this.pendingForActor(request.deviceFingerprint, request.tabId) >= ATTACHMENT_TRANSFER_PEER_MAX_PENDING_NEGOTIATIONS_PER_DEVICE
    ) throw new AttachmentPeerOfferError("capacity");
    const expectedTuple: AttachmentPeerExpectedTuple = {
      peerId: request.peerId,
      grantId: request.grantId,
      deviceFingerprint: request.deviceFingerprint,
      tabId: request.tabId,
      workerEpoch: request.workerEpoch,
    };
    const pending: PendingPeer = {
      request,
      budget,
      expectedTuple,
      config: {
        stunUrls,
        bindAddress: this.deps.bindAddress,
        portRange: this.deps.portRange,
      },
      cancelled: false,
      connection: undefined,
    };
    this.pending.set(request.requestId, pending);
    log.info("attachment-peer", "peer_negotiating", { pending: this.pending.size });
    try {
      const bootstrapState = await this.bootstrap();
      this.assertCurrent(pending);
      if (bootstrapState !== "ready" || !this.native) {
        throw new AttachmentPeerOfferError(bootstrapState === "disabled" ? "disabled" : "native_unavailable");
      }
      const peerBudget = this.packetBudget.createPeerBudget();
      let connection: AttachmentPeerConnection | undefined;
      try {
        connection = new AttachmentPeerConnection({
          native: this.native,
          peerId: request.peerId,
          expectedTuple,
          expectedRemoteFingerprint: remoteFingerprint,
          config: pending.config,
          packetBudget: peerBudget,
          openPeerPort: this.deps.openPeerPort,
          onClosed: (reason) => {
            if (connection) this.handleConnectionClosed(pending, connection, reason);
          },
        });
      } catch (error) {
        peerBudget.dispose();
        throw error;
      }
      pending.connection = connection;
      const answerSdp = await connection.answer(request.offerSdp, this.nativeAnswerDeadline(budget));
      if (connection.isClosed) throw new AttachmentPeerOfferError("ice_failed");
      this.assertCurrent(pending);
      if (this.pending.get(request.requestId) !== pending) throw new AttachmentPeerOfferError("connection_superseded");
      this.pending.delete(request.requestId);
      this.active.set(request.peerId, { expectedTuple, connection });
      log.info("attachment-peer", "peer_established", { peers: this.active.size });
      return create(WLocalAttachmentPeerAnswerSchema, {
        requestId: request.requestId,
        connectionGeneration: request.connectionGeneration,
        workerEpoch: this.deps.processEpoch,
        peerId: request.peerId,
        answerSdp,
      });
    } catch (error) {
      const reason = this.offerFailureReason(error);
      pending.connection?.close(reason === "ice_failed" ? "ice_failed" : "connection_superseded");
      if (this.pending.get(request.requestId) === pending) this.pending.delete(request.requestId);
      log.warn("attachment-peer", "peer_offer_refused", { reason, pending: this.pending.size });
      throw new AttachmentPeerOfferError(reason);
    }
  }

  cancel(request: DLocalAttachmentPeerCancel): void {
    const pending = this.pending.get(request.requestId);
    if (!pending || !this.matchesCancel(pending, request)) return;
    pending.cancelled = true;
    this.pending.delete(request.requestId);
    pending.connection?.close("connection_superseded");
    log.info("attachment-peer", "peer_cancelled", { pending: this.pending.size });
  }

  revokeDevice(deviceFingerprint: string): void {
    let closed = 0;
    for (const pending of [...this.pending.values()]) {
      if (pending.expectedTuple.deviceFingerprint !== deviceFingerprint) continue;
      pending.cancelled = true;
      this.pending.delete(pending.request.requestId);
      pending.connection?.close("connection_superseded");
      closed += 1;
    }
    for (const [peerId, active] of [...this.active]) {
      if (active.expectedTuple.deviceFingerprint !== deviceFingerprint) continue;
      this.active.delete(peerId);
      active.connection.close("connection_superseded");
      closed += 1;
    }
    if (closed > 0) log.info("attachment-peer", "peer_device_revoked", { peers: closed });
  }

  cancelPendingForCoordinator(): void {
    let cancelled = 0;
    for (const pending of [...this.pending.values()]) {
      pending.cancelled = true;
      this.pending.delete(pending.request.requestId);
      pending.connection?.close("connection_superseded");
      cancelled += 1;
    }
    if (cancelled > 0) log.info("attachment-peer", "peer_coordinator_detached", { pending: cancelled });
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.cancelPendingForCoordinator();
    for (const active of [...this.active.values()]) active.connection.close("connection_superseded");
    this.active.clear();
    this.packetBudget.dispose();
    log.info("attachment-peer", "peer_owner_disposed", {});
  }

  private async loadNative(): Promise<AttachmentPeerBootstrapState> {
    try {
      const native = await (this.deps.nativeLoader ?? loadTerminalPeerNative)();
      if (this.disposed) return "native_unavailable";
      this.native = native;
      this.bootstrapState = "ready";
      log.info("attachment-peer", "native_ready", {});
      return "ready";
    } catch {
      this.bootstrapState = "native_unavailable";
      log.warn("attachment-peer", "native_unavailable", {});
      return "native_unavailable";
    }
  }

  private admissionFailure(
    request: DLocalAttachmentPeerOffer,
    budget: TerminalRequestBudget,
  ): AttachmentPeerOfferFailureReason | null {
    if (this.disposed || request.workerEpoch !== this.deps.processEpoch) return "connection_superseded";
    if (!this.deps.enabled) return "disabled";
    if (!validAttachmentPeerOfferIdentity(request)) return "invalid_offer";
    try {
      if (!this.deps.isCurrentCoordinator(request.connectionGeneration) || !budget.isCurrentConnection()) {
        return "connection_superseded";
      }
      const remainingMs = budget.remainingMs();
      if (!Number.isFinite(remainingMs) || remainingMs <= 0) return "ice_failed";
      const grant = this.deps.authorizeGrant(request);
      return grant === "authorized" ? null : grant === "expired" ? "expired" : "grant_unavailable";
    } catch {
      return "connection_superseded";
    }
  }

  private assertCurrent(pending: PendingPeer): void {
    if (pending.cancelled || this.pending.get(pending.request.requestId) !== pending) {
      throw new AttachmentPeerOfferError("connection_superseded");
    }
    const failure = this.admissionFailure(pending.request, pending.budget);
    if (failure !== null) throw new AttachmentPeerOfferError(failure);
  }

  private nativeAnswerDeadline(budget: TerminalRequestBudget): number {
    try {
      return Math.min(ATTACHMENT_TRANSFER_PEER_NATIVE_ANSWER_DEADLINE_MS, budget.remainingMs());
    } catch {
      return 0;
    }
  }

  private hasPendingPeerId(peerId: string): boolean {
    for (const pending of this.pending.values()) {
      if (pending.request.peerId === peerId) return true;
    }
    return false;
  }

  private pendingForActor(deviceFingerprint: string, tabId: string): number {
    let pendingCount = 0;
    for (const pending of this.pending.values()) {
      if (pending.expectedTuple.deviceFingerprint === deviceFingerprint && pending.expectedTuple.tabId === tabId) {
        pendingCount += 1;
      }
    }
    return pendingCount;
  }

  private matchesCancel(pending: PendingPeer, request: DLocalAttachmentPeerCancel): boolean {
    return pending.request.connectionGeneration === request.connectionGeneration
      && pending.request.workerEpoch === request.workerEpoch
      && pending.request.peerId === request.peerId;
  }

  private handleConnectionClosed(
    pending: PendingPeer,
    connection: AttachmentPeerConnection,
    reason: AttachmentPeerConnectionFailureReason,
  ): void {
    const active = this.active.get(pending.request.peerId);
    if (active?.connection !== connection) return;
    this.active.delete(pending.request.peerId);
    log.info("attachment-peer", "peer_closed", { peers: this.active.size, reason });
  }

  private offerFailureReason(error: unknown): AttachmentPeerOfferFailureReason {
    if (error instanceof AttachmentPeerOfferError) return error.reason;
    if (error instanceof AttachmentPeerConnectionError) return error.reason;
    return "ice_failed";
  }
}
