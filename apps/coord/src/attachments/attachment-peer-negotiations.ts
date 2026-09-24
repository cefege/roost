// Owns bounded browser-to-worker attachment-peer signaling. Grant authority
// remains in AttachmentGrantOwner; this owner stores only pending operations and
// exact worker-generation fences. Typed worker answers never use generic RPC
// JSON, and credentials, SDP, and candidates never enter coordinator logs.

import { create } from "@bufbuild/protobuf";
import { createHash, randomUUID } from "node:crypto";
import type {
  SessionsNegotiateAttachmentPeerRequest,
  SessionsNegotiateAttachmentPeerResponse,
} from "@roost/protocol/proto/coordinator_pb";
import { SessionsNegotiateAttachmentPeerResponseSchema } from "@roost/protocol/proto/coordinator_pb";
import type {
  WLocalAttachmentPeerAnswer,
  WLocalAttachmentPeerError,
} from "@roost/protocol/proto/worker_transport_pb";
import {
  ATTACHMENT_TRANSFER_PEER_MAX_NEGOTIATIONS_PER_WORKER,
  ATTACHMENT_TRANSFER_PEER_MAX_PENDING_NEGOTIATIONS,
  ATTACHMENT_TRANSFER_PEER_MAX_PENDING_NEGOTIATIONS_PER_DEVICE,
  ATTACHMENT_TRANSFER_PEER_NATIVE_ANSWER_DEADLINE_MS,
  ATTACHMENT_TRANSFER_PEER_WEBRTC_CAPABILITY,
} from "@roost/protocol/attachment-transfer";
import { inspectTerminalPeerSdp } from "@roost/protocol/terminal-peer-sdp";
import { log } from "@roost/observability/log";
import type { AccountDeviceCaller } from "../auth/auth-interceptor.ts";
import type { WorkerHandle } from "../workers/worker-registry.ts";
import { currentRoutableWorker } from "../workers/worker-send-target.ts";
import type { AttachmentGrantLeaseSnapshot } from "./attachment-grant-owner-state.ts";
import { captureOwnerKey } from "../terminal/capture/terminal-capture-lease.ts";
import {
  assertAttachmentPeerRequestShape,
  attachmentPeerAlreadyExists,
  attachmentPeerCancelled,
  attachmentPeerDeadlineExceeded,
  attachmentPeerDenied,
  attachmentPeerExhausted,
  attachmentPeerInvalid,
  attachmentPeerKey,
  attachmentPeerUnavailable,
  attachmentPeerWorkerFailure,
  decrementAttachmentPeerCount,
  incrementAttachmentPeerCount,
  realAttachmentPeerNegotiationClock,
  type AttachmentPeerNegotiationClock,
  type AttachmentPeerNegotiationWorkerResultSink,
  type AttachmentPeerNegotiationsOptions,
  type PendingAttachmentPeerNegotiation,
  type AttachmentGrantInvalidation,
} from "./attachment-peer-negotiation-state.ts";
import {
  isAttachmentPeerWorkerErrorReason,
  isCurrentAttachmentPeerWorker,
  sendAttachmentPeerCancel,
  sendAttachmentPeerOffer,
} from "./worker-send-attachment-peer.ts";
/** Composition-owned attachment signaling admission and typed answer correlation. */
export class AttachmentPeerNegotiations implements AttachmentPeerNegotiationWorkerResultSink {
  private readonly pendingByRequestId = new Map<string, PendingAttachmentPeerNegotiation>();
  private readonly pendingByOwnerTabWorker = new Map<string, PendingAttachmentPeerNegotiation>();
  private readonly admittingByOwnerTabWorker = new Map<string, {
    peerId: string;
    offerDigest: string;
    deviceFingerprint: string;
    workerFp: string;
  }>();
  private readonly pendingByDevice = new Map<string, number>();
  private readonly pendingByWorker = new Map<string, number>();
  private readonly currentWorker: (workerFp: string) => WorkerHandle | null;
  private readonly clock: AttachmentPeerNegotiationClock;
  private readonly createRequestId: () => string;
  private readonly removeGrantInvalidation: () => void;
  private readonly answerTimeoutMs: number;
  private disposed = false;
  constructor(private readonly options: AttachmentPeerNegotiationsOptions) {
    this.currentWorker = options.currentWorker ?? currentRoutableWorker;
    this.clock = options.clock ?? realAttachmentPeerNegotiationClock;
    this.createRequestId = options.createRequestId ?? randomUUID;
    this.answerTimeoutMs = options.answerTimeoutMs ?? ATTACHMENT_TRANSFER_PEER_NATIVE_ANSWER_DEADLINE_MS;
    if (!Number.isSafeInteger(this.answerTimeoutMs) || this.answerTimeoutMs <= 0) {
      throw new RangeError("attachment peer answer timeout must be a positive safe integer");
    }
    this.removeGrantInvalidation = options.attachmentGrants.subscribeInvalidation((invalidation) => {
      this.cancelInvalidated(invalidation);
    });
  }
  async negotiate(
    caller: AccountDeviceCaller,
    authenticatedTabId: string | undefined,
    request: SessionsNegotiateAttachmentPeerRequest,
    signal: AbortSignal,
  ): Promise<SessionsNegotiateAttachmentPeerResponse> {
    assertAttachmentPeerRequestShape(request);
    if (this.disposed) throw attachmentPeerUnavailable("attachment peer signaling is unavailable");
    if (!authenticatedTabId || request.tabId !== authenticatedTabId) {
      throw attachmentPeerDenied("attachment peer tab does not match the authenticated document");
    }
    if (signal.aborted) throw attachmentPeerCancelled("attachment peer negotiation cancelled");
    const ownerKey = captureOwnerKey(caller);
    const offerDigest = createHash("sha256").update(request.offerSdp).digest("hex");
    const admissionKey = attachmentPeerKey(ownerKey, request.tabId, request.workerFp);
    this.claimAdmission(admissionKey, request.peerId, offerDigest, caller.fingerprint, request.workerFp);
    let admissionHeld = true;
    try {
      try {
        inspectTerminalPeerSdp(request.offerSdp);
      } catch {
        throw attachmentPeerInvalid("attachment peer offer is invalid");
      }
      const grant = this.requireOwnedGrant(ownerKey, request);
      if (this.disposed) throw attachmentPeerUnavailable("attachment peer signaling is unavailable");
      if (signal.aborted) throw attachmentPeerCancelled("attachment peer negotiation cancelled");
      const stableGrant = this.requireStableGrant(ownerKey, request, grant);
      const worker = this.requireWorker(request, stableGrant);
      const pending = this.reserve(caller, ownerKey, request, stableGrant, worker, offerDigest, signal);
      this.admittingByOwnerTabWorker.delete(admissionKey);
      admissionHeld = false;
      if (signal.aborted) {
        this.cancelPending(pending, attachmentPeerCancelled("attachment peer negotiation cancelled"), true);
        return pending.promise;
      }
      const budgetMs = Math.floor(pending.deadlineAtMono - this.clock.now());
      if (budgetMs <= 0) {
        this.cancelPending(pending, attachmentPeerDeadlineExceeded("attachment peer answer timed out"), true);
        return pending.promise;
      }
      if (!sendAttachmentPeerOffer(worker, {
        requestId: pending.requestId,
        grantId: pending.grantId,
        peerId: pending.peerId,
        deviceFingerprint: pending.deviceFingerprint,
        tabId: pending.tabId,
        workerEpoch: pending.workerEpoch,
        offerSdp: request.offerSdp,
        budgetMs,
        stunUrls: this.options.cfg.terminalPeerStunUrls,
      })) {
        this.cancelPending(pending, attachmentPeerUnavailable("attachment peer worker is unavailable"), false);
        return pending.promise;
      }
      log.debug("attachment-peer-negotiations", "offer_sent", {
        worker_fp: pending.workerFp,
        pending: this.pendingByRequestId.size,
      });
      const response = await pending.promise;
      this.requireWorker(request, this.requireStableGrant(ownerKey, request, stableGrant));
      return response;
    } finally {
      if (admissionHeld) this.releaseAdmission(admissionKey);
    }
  }
  acceptAnswer(source: WorkerHandle, answer: WLocalAttachmentPeerAnswer): boolean {
    const pending = this.pendingByRequestId.get(answer.requestId);
    if (!pending || !this.matchesPending(source, pending, answer.connectionGeneration, answer.workerEpoch, answer.peerId)) {
      return false;
    }
    try {
      inspectTerminalPeerSdp(answer.answerSdp);
    } catch {
      this.cancelPending(pending, attachmentPeerUnavailable("attachment peer worker returned an invalid answer"), true);
      return true;
    }
    if (!this.removePending(pending)) return false;
    pending.resolve(create(SessionsNegotiateAttachmentPeerResponseSchema, {
      peerId: pending.peerId,
      answerSdp: answer.answerSdp,
      workerEpoch: pending.workerEpoch,
    }));
    log.debug("attachment-peer-negotiations", "answer_accepted", {
      worker_fp: pending.workerFp,
      pending: this.pendingByRequestId.size,
    });
    return true;
  }
  acceptError(source: WorkerHandle, error: WLocalAttachmentPeerError): boolean {
    const pending = this.pendingByRequestId.get(error.requestId);
    if (
      !pending || !isAttachmentPeerWorkerErrorReason(error.reason)
      || !this.matchesPending(source, pending, error.connectionGeneration, error.workerEpoch, error.peerId)
    ) return false;
    this.cancelPending(pending, attachmentPeerWorkerFailure(error.reason), false);
    return true;
  }
  cancelForWorkerHandle(worker: WorkerHandle, _reason: string): void {
    for (const pending of [...this.pendingByRequestId.values()]) {
      if (pending.worker === worker) {
        this.cancelPending(pending, attachmentPeerUnavailable("attachment peer worker connection changed"), true);
      }
    }
  }
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.removeGrantInvalidation();
    for (const key of [...this.admittingByOwnerTabWorker.keys()]) this.releaseAdmission(key);
    for (const pending of [...this.pendingByRequestId.values()]) {
      this.cancelPending(pending, attachmentPeerUnavailable("attachment peer signaling is unavailable"), true);
    }
  }
  private requireOwnedGrant(
    ownerKey: string,
    request: SessionsNegotiateAttachmentPeerRequest,
  ): AttachmentGrantLeaseSnapshot {
    const grant = this.options.attachmentGrants.ownedGrant(
      ownerKey,
      request.tabId,
      request.workerFp,
      request.grantId,
    );
    if (!grant) throw attachmentPeerDenied("attachment peer grant is unavailable");
    return grant;
  }
  private requireStableGrant(
    ownerKey: string,
    request: SessionsNegotiateAttachmentPeerRequest,
    expected: AttachmentGrantLeaseSnapshot,
  ): AttachmentGrantLeaseSnapshot {
    const current = this.requireOwnedGrant(ownerKey, request);
    if (
      current.grantId !== expected.grantId
      || current.workerHandle !== expected.workerHandle
      || current.workerEpoch !== expected.workerEpoch
      || current.descriptor.sessionId !== expected.descriptor.sessionId
      || current.descriptor.uploadId !== expected.descriptor.uploadId
    ) throw attachmentPeerDenied("attachment peer grant changed during negotiation");
    return current;
  }
  private requireWorker(
    request: SessionsNegotiateAttachmentPeerRequest,
    grant: AttachmentGrantLeaseSnapshot,
  ): WorkerHandle {
    const worker = this.currentWorker(request.workerFp);
    if (
      !this.options.cfg.terminalPeerEnabled || !worker || worker !== grant.workerHandle
      || worker.processEpoch === null || worker.processEpoch !== grant.workerEpoch
      || worker.processEpoch !== request.workerEpoch
      || !worker.capabilities.has(ATTACHMENT_TRANSFER_PEER_WEBRTC_CAPABILITY)
      || !isCurrentAttachmentPeerWorker(worker, request.workerEpoch)
    ) throw attachmentPeerUnavailable("attachment peer worker is unavailable");
    return worker;
  }

  private reserve(
    caller: AccountDeviceCaller,
    ownerKey: string,
    request: SessionsNegotiateAttachmentPeerRequest,
    grant: AttachmentGrantLeaseSnapshot,
    worker: WorkerHandle,
    offerDigest: string,
    signal: AbortSignal,
  ): PendingAttachmentPeerNegotiation {
    const deferred = Promise.withResolvers<SessionsNegotiateAttachmentPeerResponse>();
    const pending: PendingAttachmentPeerNegotiation = {
      requestId: this.allocateRequestId(),
      ownerKey,
      deviceFingerprint: caller.fingerprint,
      tabId: request.tabId,
      workerFp: request.workerFp,
      grantId: grant.grantId,
      peerId: request.peerId,
      worker,
      connectionGeneration: worker.connectionGeneration,
      workerEpoch: request.workerEpoch,
      offerDigest,
      deadlineAtMono: this.clock.now() + this.answerTimeoutMs,
      signal,
      promise: deferred.promise,
      resolve: deferred.resolve,
      reject: deferred.reject,
      abortListener: () => {},
      timer: null,
    };
    pending.abortListener = () => {
      this.cancelPending(pending, attachmentPeerCancelled("attachment peer negotiation cancelled"), true);
    };
    this.pendingByRequestId.set(pending.requestId, pending);
    this.pendingByOwnerTabWorker.set(attachmentPeerKey(ownerKey, request.tabId, request.workerFp), pending);
    signal.addEventListener("abort", pending.abortListener, { once: true });
    pending.timer = this.clock.setTimeout(() => {
      this.cancelPending(pending, attachmentPeerDeadlineExceeded("attachment peer answer timed out"), true);
    }, this.answerTimeoutMs);
    pending.timer.unref?.();
    return pending;
  }

  private claimAdmission(
    key: string,
    peerId: string,
    offerDigest: string,
    deviceFingerprint: string,
    workerFp: string,
  ): void {
    const existing = this.admittingByOwnerTabWorker.get(key) ?? this.pendingByOwnerTabWorker.get(key);
    if (existing) {
      if (existing.peerId === peerId && existing.offerDigest !== offerDigest) {
        throw attachmentPeerInvalid("attachment peer peer_id conflicts with an in-flight offer");
      }
      throw attachmentPeerAlreadyExists("attachment peer negotiation is already pending for this document and worker");
    }
    if (this.pendingByRequestId.size + this.admittingByOwnerTabWorker.size >= ATTACHMENT_TRANSFER_PEER_MAX_PENDING_NEGOTIATIONS) {
      throw attachmentPeerExhausted("attachment peer negotiation capacity is exhausted");
    }
    if ((this.pendingByDevice.get(deviceFingerprint) ?? 0) >= ATTACHMENT_TRANSFER_PEER_MAX_PENDING_NEGOTIATIONS_PER_DEVICE) {
      throw attachmentPeerExhausted("attachment peer device negotiation capacity is exhausted");
    }
    if ((this.pendingByWorker.get(workerFp) ?? 0) >= ATTACHMENT_TRANSFER_PEER_MAX_NEGOTIATIONS_PER_WORKER) {
      throw attachmentPeerExhausted("attachment peer worker negotiation capacity is exhausted");
    }
    this.admittingByOwnerTabWorker.set(key, { peerId, offerDigest, deviceFingerprint, workerFp });
    incrementAttachmentPeerCount(this.pendingByDevice, deviceFingerprint);
    incrementAttachmentPeerCount(this.pendingByWorker, workerFp);
  }

  private releaseAdmission(key: string): void {
    const admission = this.admittingByOwnerTabWorker.get(key);
    if (!admission) return;
    this.admittingByOwnerTabWorker.delete(key);
    decrementAttachmentPeerCount(this.pendingByDevice, admission.deviceFingerprint);
    decrementAttachmentPeerCount(this.pendingByWorker, admission.workerFp);
  }

  private allocateRequestId(): string {
    for (let attempt = 0; attempt < 8; attempt += 1) {
      const requestId = this.createRequestId();
      if (requestId && !this.pendingByRequestId.has(requestId)) return requestId;
    }
    throw attachmentPeerExhausted("attachment peer request capacity is exhausted");
  }

  private matchesPending(
    source: WorkerHandle,
    pending: PendingAttachmentPeerNegotiation,
    connectionGeneration: string,
    workerEpoch: string,
    peerId: string,
  ): boolean {
    return (
      source === pending.worker && source.connectionGeneration === pending.connectionGeneration
      && isCurrentAttachmentPeerWorker(source, pending.workerEpoch)
      && connectionGeneration === pending.connectionGeneration
      && workerEpoch === pending.workerEpoch && peerId === pending.peerId
    );
  }

  private cancelInvalidated(invalidation: AttachmentGrantInvalidation): void {
    const lease = invalidation.lease;
    for (const pending of [...this.pendingByRequestId.values()]) {
      const matches = invalidation.kind === "disposed"
        || (invalidation.kind === "worker_retired" && invalidation.workerFp === pending.workerFp)
        || (invalidation.kind === "device_revoked" && invalidation.deviceFingerprint === pending.deviceFingerprint)
        || (
          lease !== null && lease.grantId === pending.grantId
          && lease.ownerKey === pending.ownerKey && lease.tabId === pending.tabId
          && lease.workerFp === pending.workerFp
        );
      if (matches) {
        this.cancelPending(pending, attachmentPeerDenied("attachment peer grant is unavailable"), true);
      }
    }
  }

  private cancelPending(
    pending: PendingAttachmentPeerNegotiation,
    error: Error,
    sendCancel: boolean,
  ): void {
    if (!this.removePending(pending)) return;
    if (sendCancel) {
      sendAttachmentPeerCancel(pending.worker, {
        requestId: pending.requestId,
        peerId: pending.peerId,
        workerEpoch: pending.workerEpoch,
      });
    }
    pending.reject(error);
    log.debug("attachment-peer-negotiations", "pending_cancelled", {
      worker_fp: pending.workerFp,
      pending: this.pendingByRequestId.size,
    });
  }

  private removePending(pending: PendingAttachmentPeerNegotiation): boolean {
    if (this.pendingByRequestId.get(pending.requestId) !== pending) return false;
    this.pendingByRequestId.delete(pending.requestId);
    const key = attachmentPeerKey(pending.ownerKey, pending.tabId, pending.workerFp);
    if (this.pendingByOwnerTabWorker.get(key) === pending) this.pendingByOwnerTabWorker.delete(key);
    decrementAttachmentPeerCount(this.pendingByDevice, pending.deviceFingerprint);
    decrementAttachmentPeerCount(this.pendingByWorker, pending.workerFp);
    if (pending.timer !== null) this.clock.clearTimeout(pending.timer);
    pending.timer = null;
    pending.signal.removeEventListener("abort", pending.abortListener);
    return true;
  }
}
