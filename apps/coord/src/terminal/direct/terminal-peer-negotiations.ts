// Owns bounded browser-to-worker terminal-peer signaling. Session authority
// remains in TerminalGrantOwner; this owner retains only pending operations,
// exact worker-generation fences, and cancellation lifecycle. Worker answers
// are typed frames, never generic RPC JSON, and SDP never reaches logs.

import { create } from "@bufbuild/protobuf";
import { createHash, randomUUID } from "node:crypto";
import type {
  SessionsNegotiateLocalTerminalPeerRequest,
  SessionsNegotiateLocalTerminalPeerResponse,
} from "@roost/protocol/proto/coordinator_pb";
import { SessionsNegotiateLocalTerminalPeerResponseSchema } from "@roost/protocol/proto/coordinator_pb";
import type {
  WLocalTerminalPeerAnswer,
  WLocalTerminalPeerError,
} from "@roost/protocol/proto/worker_transport_pb";
import {
  TERMINAL_PEER_MAX_NEGOTIATIONS_PER_WORKER,
  TERMINAL_PEER_MAX_PENDING_NEGOTIATIONS,
  TERMINAL_PEER_MAX_PENDING_NEGOTIATIONS_PER_DEVICE,
  TERMINAL_PEER_NATIVE_ANSWER_DEADLINE_MS,
  TERMINAL_PEER_WEBRTC_CAPABILITY,
} from "@roost/protocol/terminal-peer";
import { inspectTerminalPeerSdp } from "@roost/protocol/terminal-peer-sdp";
import { log } from "@roost/observability/log";
import type { AccountDeviceCaller } from "../../auth/auth-interceptor.ts";
import type { WorkerHandle } from "../../workers/worker-registry.ts";
import { currentRoutableWorker } from "../../workers/worker-send-target.ts";
import { authorizeTerminalGrantSessions } from "./local-terminal-grants.ts";
import type { TerminalGrantLeaseSnapshot } from "./terminal-grant-owner.ts";
import { captureOwnerKey } from "../capture/terminal-capture-lease.ts";
import {
  assertTerminalPeerRequestShape,
  decrementTerminalPeerCount,
  hasValidTerminalPeerGrantSessions,
  incrementTerminalPeerCount,
  realTerminalPeerNegotiationClock,
  terminalPeerAlreadyExists,
  terminalPeerCancelled,
  terminalPeerDeadlineExceeded,
  terminalPeerDenied,
  terminalPeerExhausted,
  terminalPeerInvalid,
  terminalPeerKey,
  terminalPeerUnavailable,
  type PendingTerminalPeerNegotiation,
  type TerminalGrantInvalidation,
  type TerminalGrantSessionAuthorizer,
  terminalPeerWorkerFailure,
  type TerminalPeerNegotiationClock,
  type TerminalPeerNegotiationWorkerResultSink,
  type TerminalPeerNegotiationsOptions,
} from "./terminal-peer-negotiation-state.ts";
import {
  isCurrentTerminalPeerWorker,
  isTerminalPeerWorkerErrorReason,
  sendTerminalPeerCancel,
  sendTerminalPeerOffer,
} from "./worker-send-terminal-peer.ts";
/** Composition-owned signaling admission and typed-answer correlation. */
export class TerminalPeerNegotiations implements TerminalPeerNegotiationWorkerResultSink {
  private readonly pendingByRequestId = new Map<string, PendingTerminalPeerNegotiation>();
  private readonly pendingByOwnerTabWorker = new Map<string, PendingTerminalPeerNegotiation>();
  private readonly admittingByOwnerTabWorker = new Map<string, { peerId: string; offerDigest: string; deviceFingerprint: string; workerFp: string }>();
  private readonly pendingByDevice = new Map<string, number>();
  private readonly pendingByWorker = new Map<string, number>();
  private readonly currentWorker: (workerFp: string) => WorkerHandle | null;
  private readonly authorizeSessions: TerminalGrantSessionAuthorizer;
  private readonly clock: TerminalPeerNegotiationClock;
  private readonly createRequestId: () => string;
  private readonly removeGrantInvalidation: () => void;
  private readonly answerTimeoutMs: number;
  private disposed = false;
  constructor(private readonly options: TerminalPeerNegotiationsOptions) {
    this.currentWorker = options.currentWorker ?? currentRoutableWorker;
    this.authorizeSessions = options.authorizeSessions ?? authorizeTerminalGrantSessions;
    this.clock = options.clock ?? realTerminalPeerNegotiationClock;
    this.createRequestId = options.createRequestId ?? randomUUID;
    this.answerTimeoutMs = options.answerTimeoutMs ?? TERMINAL_PEER_NATIVE_ANSWER_DEADLINE_MS;
    if (!Number.isSafeInteger(this.answerTimeoutMs) || this.answerTimeoutMs <= 0) {
      throw new RangeError("terminal peer answer timeout must be a positive safe integer");
    }
    this.removeGrantInvalidation = options.terminalGrants.subscribeInvalidation((invalidation) => {
      this.cancelInvalidated(invalidation);
    });
  }
  async negotiate(
    caller: AccountDeviceCaller,
    authenticatedTabId: string | undefined,
    request: SessionsNegotiateLocalTerminalPeerRequest,
    signal: AbortSignal,
  ): Promise<SessionsNegotiateLocalTerminalPeerResponse> {
    assertTerminalPeerRequestShape(request);
    if (this.disposed) throw terminalPeerUnavailable("terminal peer signaling is unavailable");
    if (!authenticatedTabId || request.tabId !== authenticatedTabId) {
      throw terminalPeerDenied("terminal peer tab does not match the authenticated document");
    }
    if (signal.aborted) throw terminalPeerCancelled("terminal peer negotiation cancelled");
    const ownerKey = captureOwnerKey(caller);
    const offerDigest = createHash("sha256").update(request.offerSdp).digest("hex");
    const admissionKey = terminalPeerKey(ownerKey, request.tabId, request.workerFp);
    this.claimAdmission(admissionKey, request.peerId, offerDigest, caller.fingerprint, request.workerFp);
    let admissionHeld = true;
    try {
      try {
        inspectTerminalPeerSdp(request.offerSdp);
      } catch {
        throw terminalPeerInvalid("terminal peer offer is invalid");
      }
      const grant = this.requireOwnedGrant(ownerKey, request);
      await this.authorizeSessions(this.options.db, request.workerFp, grant.sessionIds);
      if (this.disposed) throw terminalPeerUnavailable("terminal peer signaling is unavailable");
      if (signal.aborted) throw terminalPeerCancelled("terminal peer negotiation cancelled");
      const stableGrant = this.requireStableGrant(ownerKey, request, grant);
      const worker = this.requireWorker(request, stableGrant);
      const pending = this.reserve(caller, ownerKey, request, stableGrant, worker, offerDigest, signal);
      this.admittingByOwnerTabWorker.delete(admissionKey);
      admissionHeld = false;
      if (signal.aborted) {
        this.cancelPending(pending, terminalPeerCancelled("terminal peer negotiation cancelled"), "aborted", false);
        return pending.promise;
      }
      const budgetMs = Math.floor(pending.deadlineAtMono - this.clock.now());
      if (budgetMs <= 0) {
        this.cancelPending(pending, terminalPeerDeadlineExceeded("terminal peer answer timed out"), "timeout", false);
        return pending.promise;
      }
      if (!sendTerminalPeerOffer(worker, {
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
        this.cancelPending(pending, terminalPeerUnavailable("terminal peer worker is unavailable"), "send_failed", false);
        return pending.promise;
      }
      log.debug("terminal-peer-negotiations", "offer_sent", {
        worker_fp: pending.workerFp,
        pending: this.pendingByRequestId.size,
      });
      const response = await pending.promise;
      const currentGrant = this.requireStableGrant(ownerKey, request, stableGrant);
      await this.authorizeSessions(this.options.db, request.workerFp, currentGrant.sessionIds);
      this.requireWorker(request, this.requireStableGrant(ownerKey, request, currentGrant));
      return response;
    } finally {
      if (admissionHeld) this.releaseAdmission(admissionKey);
    }
  }
  acceptAnswer(source: WorkerHandle, answer: WLocalTerminalPeerAnswer): boolean {
    const pending = this.pendingByRequestId.get(answer.requestId);
    if (!pending || !this.matchesPending(source, pending, answer.connectionGeneration, answer.workerEpoch, answer.peerId)) {
      return false;
    }
    try {
      inspectTerminalPeerSdp(answer.answerSdp);
    } catch {
      this.cancelPending(pending, terminalPeerUnavailable("terminal peer worker returned an invalid answer"), "invalid_answer", true);
      return true;
    }
    if (!this.removePending(pending)) return false;
    pending.resolve(create(SessionsNegotiateLocalTerminalPeerResponseSchema, {
      peerId: pending.peerId,
      answerSdp: answer.answerSdp,
      workerEpoch: pending.workerEpoch,
    }));
    log.debug("terminal-peer-negotiations", "answer_accepted", {
      worker_fp: pending.workerFp,
      pending: this.pendingByRequestId.size,
    });
    return true;
  }
  acceptError(source: WorkerHandle, error: WLocalTerminalPeerError): boolean {
    const pending = this.pendingByRequestId.get(error.requestId);
    if (
      !pending || !isTerminalPeerWorkerErrorReason(error.reason)
      || !this.matchesPending(source, pending, error.connectionGeneration, error.workerEpoch, error.peerId)
    ) return false;
    this.cancelPending(pending, terminalPeerWorkerFailure(error.reason), error.reason, false);
    return true;
  }
  cancelForWorkerHandle(worker: WorkerHandle, reason: string): void {
    for (const pending of [...this.pendingByRequestId.values()]) {
      if (pending.worker === worker) {
        this.cancelPending(pending, terminalPeerUnavailable("terminal peer worker connection changed"), reason, true);
      }
    }
  }
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.removeGrantInvalidation();
    for (const key of [...this.admittingByOwnerTabWorker.keys()]) this.releaseAdmission(key);
    for (const pending of [...this.pendingByRequestId.values()]) {
      this.cancelPending(pending, terminalPeerUnavailable("terminal peer signaling is unavailable"), "disposed", true);
    }
  }
  private requireOwnedGrant(
    ownerKey: string,
    request: SessionsNegotiateLocalTerminalPeerRequest,
  ): TerminalGrantLeaseSnapshot {
    const grant = this.options.terminalGrants.ownedGrant(ownerKey, request.tabId, request.workerFp, request.grantId);
    if (!grant || !hasValidTerminalPeerGrantSessions(grant.sessionIds)) {
      throw terminalPeerDenied("terminal peer grant is unavailable");
    }
    return grant;
  }
  private requireStableGrant(
    ownerKey: string,
    request: SessionsNegotiateLocalTerminalPeerRequest,
    expected: TerminalGrantLeaseSnapshot,
  ): TerminalGrantLeaseSnapshot {
    const current = this.requireOwnedGrant(ownerKey, request);
    if (
      current.grantId !== expected.grantId
      || current.workerHandle !== expected.workerHandle
      || current.workerEpoch !== expected.workerEpoch
      || current.sessionIds.length !== expected.sessionIds.length
      || current.sessionIds.some((sessionId, index) => sessionId !== expected.sessionIds[index])
    ) throw terminalPeerDenied("terminal peer grant changed during negotiation");
    return current;
  }
  private requireWorker(
    request: SessionsNegotiateLocalTerminalPeerRequest,
    grant: TerminalGrantLeaseSnapshot,
  ): WorkerHandle {
    const worker = this.currentWorker(request.workerFp);
    if (
      !this.options.cfg.terminalPeerEnabled || !worker || worker !== grant.workerHandle
      || worker.processEpoch === null || worker.processEpoch !== grant.workerEpoch
      || worker.processEpoch !== request.workerEpoch
      || !worker.capabilities.has(TERMINAL_PEER_WEBRTC_CAPABILITY)
      || !isCurrentTerminalPeerWorker(worker, request.workerEpoch)
    ) throw terminalPeerUnavailable("terminal peer worker is unavailable");
    return worker;
  }
  private reserve(
    caller: AccountDeviceCaller,
    ownerKey: string,
    request: SessionsNegotiateLocalTerminalPeerRequest,
    grant: TerminalGrantLeaseSnapshot,
    worker: WorkerHandle,
    offerDigest: string,
    signal: AbortSignal,
  ): PendingTerminalPeerNegotiation {
    const deferred = Promise.withResolvers<SessionsNegotiateLocalTerminalPeerResponse>();
    const pending: PendingTerminalPeerNegotiation = {
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
      this.cancelPending(pending, terminalPeerCancelled("terminal peer negotiation cancelled"), "aborted", true);
    };
    this.pendingByRequestId.set(pending.requestId, pending);
    this.pendingByOwnerTabWorker.set(terminalPeerKey(ownerKey, request.tabId, request.workerFp), pending);
    signal.addEventListener("abort", pending.abortListener, { once: true });
    pending.timer = this.clock.setTimeout(() => {
      this.cancelPending(pending, terminalPeerDeadlineExceeded("terminal peer answer timed out"), "timeout", true);
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
        throw terminalPeerInvalid("terminal peer peer_id conflicts with an in-flight offer");
      }
      throw terminalPeerAlreadyExists("terminal peer negotiation is already pending for this document and worker");
    }
    if (this.pendingByRequestId.size + this.admittingByOwnerTabWorker.size >= TERMINAL_PEER_MAX_PENDING_NEGOTIATIONS) {
      throw terminalPeerExhausted("terminal peer negotiation capacity is exhausted");
    }
    if ((this.pendingByDevice.get(deviceFingerprint) ?? 0) >= TERMINAL_PEER_MAX_PENDING_NEGOTIATIONS_PER_DEVICE) {
      throw terminalPeerExhausted("terminal peer device negotiation capacity is exhausted");
    }
    if ((this.pendingByWorker.get(workerFp) ?? 0) >= TERMINAL_PEER_MAX_NEGOTIATIONS_PER_WORKER) {
      throw terminalPeerExhausted("terminal peer worker negotiation capacity is exhausted");
    }
    this.admittingByOwnerTabWorker.set(key, { peerId, offerDigest, deviceFingerprint, workerFp });
    incrementTerminalPeerCount(this.pendingByDevice, deviceFingerprint);
    incrementTerminalPeerCount(this.pendingByWorker, workerFp);
  }
  private releaseAdmission(key: string): void {
    const admission = this.admittingByOwnerTabWorker.get(key);
    if (!admission) return;
    this.admittingByOwnerTabWorker.delete(key);
    decrementTerminalPeerCount(this.pendingByDevice, admission.deviceFingerprint);
    decrementTerminalPeerCount(this.pendingByWorker, admission.workerFp);
  }
  private allocateRequestId(): string {
    for (let attempt = 0; attempt < 8; attempt += 1) {
      const requestId = this.createRequestId();
      if (requestId && !this.pendingByRequestId.has(requestId)) return requestId;
    }
    throw terminalPeerExhausted("terminal peer request capacity is exhausted");
  }
  private matchesPending(
    source: WorkerHandle,
    pending: PendingTerminalPeerNegotiation,
    connectionGeneration: string,
    workerEpoch: string,
    peerId: string,
  ): boolean {
    return (
      source === pending.worker && source.connectionGeneration === pending.connectionGeneration
      && isCurrentTerminalPeerWorker(source, pending.workerEpoch)
      && connectionGeneration === pending.connectionGeneration
      && workerEpoch === pending.workerEpoch && peerId === pending.peerId
    );
  }
  private cancelInvalidated(invalidation: TerminalGrantInvalidation): void {
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
        this.cancelPending(pending, terminalPeerDenied("terminal peer grant is unavailable"), invalidation.kind, true);
      }
    }
  }
  private cancelPending(
    pending: PendingTerminalPeerNegotiation,
    error: Error,
    reason: string,
    sendCancel: boolean,
  ): void {
    if (!this.removePending(pending)) return;
    if (sendCancel) {
      sendTerminalPeerCancel(pending.worker, {
        requestId: pending.requestId,
        peerId: pending.peerId,
        workerEpoch: pending.workerEpoch,
      });
    }
    pending.reject(error);
    log.debug("terminal-peer-negotiations", "pending_cancelled", {
      worker_fp: pending.workerFp,
      reason,
      pending: this.pendingByRequestId.size,
    });
  }
  private removePending(pending: PendingTerminalPeerNegotiation): boolean {
    if (this.pendingByRequestId.get(pending.requestId) !== pending) return false;
    this.pendingByRequestId.delete(pending.requestId);
    const key = terminalPeerKey(pending.ownerKey, pending.tabId, pending.workerFp);
    if (this.pendingByOwnerTabWorker.get(key) === pending) this.pendingByOwnerTabWorker.delete(key);
    decrementTerminalPeerCount(this.pendingByDevice, pending.deviceFingerprint);
    decrementTerminalPeerCount(this.pendingByWorker, pending.workerFp);
    if (pending.timer !== null) this.clock.clearTimeout(pending.timer);
    pending.timer = null;
    pending.signal.removeEventListener("abort", pending.abortListener);
    return true;
  }
}
