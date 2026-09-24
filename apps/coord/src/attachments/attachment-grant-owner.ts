// Owns separate short-lived attachment grants and their worker-acknowledged
// installation. A lease binds one immutable upload descriptor to one device,
// tab, worker handle, and epoch; it never shares terminal-grant authority.
// Peer signaling reads only a live exact lease, while workers receive digests.

import { createHash, randomBytes, randomUUID } from "node:crypto";
import { log } from "@roost/observability/log";
import {
  ATTACHMENT_TRANSFER_GRANT_TTL_MS,
  ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_BROWSER_DOCUMENT,
  ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_WORKER,
  ATTACHMENT_TRANSFER_MAX_PENDING_GRANTS,
  ATTACHMENT_TRANSFER_MAX_PENDING_GRANTS_PER_DEVICE,
} from "@roost/protocol/attachment-transfer";
import { connectWorkers } from "../workers/worker-registry.ts";
import { currentRoutableWorker } from "../workers/worker-send-target.ts";
import {
  assertAttachmentGrantRequest,
  attachmentGrantExhausted,
  attachmentGrantSnapshot,
  attachmentGrantUnavailable,
  isExactAttachmentGrantWorker,
  type AttachmentGrantInvalidation,
  type AttachmentGrantLease,
  type AttachmentGrantLeaseSnapshot,
  type AttachmentGrantPort,
  type AttachmentGrantRequest,
  type AttachmentGrantResult,
  type AttachmentGrantRetireReason,
  type PendingAttachmentGrant,
} from "./attachment-grant-owner-state.ts";
import {
  sendLocalAttachmentGrantRequest,
  sendLocalAttachmentGrantRevoke,
} from "./worker-send-attachment-grant.ts";

export type {
  AttachmentGrantDescriptor,
  AttachmentGrantInvalidation,
  AttachmentGrantLeaseSnapshot,
  AttachmentGrantRequest,
  AttachmentGrantResult,
  AttachmentGrantRetireReason,
} from "./attachment-grant-owner-state.ts";

export interface AttachmentGrantOwnerOptions {
  readonly now?: () => number;
}

/** One composition-owned registry for immutable direct attachment credentials. */
export class AttachmentGrantOwner implements AttachmentGrantPort {
  readonly #leases = new Map<string, AttachmentGrantLease>();
  readonly #listeners = new Set<(invalidation: AttachmentGrantInvalidation) => void>();
  readonly #pending = new Map<string, PendingAttachmentGrant>();
  readonly #pendingByOwnerTab = new Map<string, number>();
  readonly #pendingByWorker = new Map<string, number>();
  readonly #pendingByDevice = new Map<string, number>();
  readonly #leasesByOwnerTab = new Map<string, number>();
  readonly #leasesByWorker = new Map<string, number>();
  readonly #now: () => number;
  #disposed = false;

  constructor(options: AttachmentGrantOwnerOptions = {}) {
    this.#now = options.now ?? Date.now;
  }

  /** Returns a lease only while the originally authorized handle remains current. */
  ownedGrant(
    ownerKey: string,
    tabId: string,
    workerFp: string,
    grantId: string,
  ): AttachmentGrantLeaseSnapshot | null {
    this.sweep(this.#now());
    const lease = this.#leases.get(grantId);
    if (!lease || lease.ownerKey !== ownerKey || lease.tabId !== tabId || lease.workerFp !== workerFp) {
      return null;
    }
    if (!isExactAttachmentGrantWorker(lease.workerHandle, lease.workerEpoch)) return null;
    return attachmentGrantSnapshot(lease);
  }

  /** Installs a new immutable credential; the plaintext secret returns only after worker ACK. */
  async grant(request: AttachmentGrantRequest): Promise<AttachmentGrantResult> {
    this.assertOpen();
    assertAttachmentGrantRequest(request);
    const descriptor = Object.freeze({ ...request.descriptor });
    this.sweep(this.#now());
    const ownerTabKey = JSON.stringify([request.ownerKey, request.tabId]);
    if (
      (this.#leasesByOwnerTab.get(ownerTabKey) ?? 0) + (this.#pendingByOwnerTab.get(ownerTabKey) ?? 0)
      >= ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_BROWSER_DOCUMENT
    ) {
      throw attachmentGrantExhausted("attachment grant document capacity is exhausted");
    }
    if (
      (this.#leasesByWorker.get(request.workerFp) ?? 0) + (this.#pendingByWorker.get(request.workerFp) ?? 0)
      >= ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_WORKER
    ) {
      throw attachmentGrantExhausted("attachment grant worker capacity is exhausted");
    }
    if (this.#pending.size >= ATTACHMENT_TRANSFER_MAX_PENDING_GRANTS) {
      throw attachmentGrantExhausted("attachment grant capacity is exhausted");
    }
    if ((this.#pendingByDevice.get(request.deviceFingerprint) ?? 0) >= ATTACHMENT_TRANSFER_MAX_PENDING_GRANTS_PER_DEVICE) {
      throw attachmentGrantExhausted("attachment grant device capacity is exhausted");
    }
    const worker = currentRoutableWorker(request.workerFp);
    const workerEpoch = worker?.processEpoch;
    if (!worker || workerEpoch === null || workerEpoch === undefined || !isExactAttachmentGrantWorker(worker, workerEpoch)) {
      throw attachmentGrantUnavailable("attachment grant worker is unavailable");
    }
    const grantId = randomUUID();
    const pending: PendingAttachmentGrant = {
      grantId,
      deviceFingerprint: request.deviceFingerprint,
      workerFp: request.workerFp,
      invalidated: false,
    };
    this.#pending.set(grantId, pending);
    this.increment(this.#pendingByDevice, request.deviceFingerprint);
    this.increment(this.#pendingByOwnerTab, ownerTabKey);
    this.increment(this.#pendingByWorker, request.workerFp);
    try {
      await request.authorize();
      this.assertPendingLive(pending);
      if (!isExactAttachmentGrantWorker(worker, workerEpoch)) {
        throw attachmentGrantUnavailable("attachment grant worker is unavailable");
      }
      const secret = randomBytes(32).toString("hex");
      const install = sendLocalAttachmentGrantRequest(worker, workerEpoch, {
        grantId,
        secretSha256: createHash("sha256").update(secret).digest("hex"),
        descriptor,
        deviceFingerprint: request.deviceFingerprint,
        tabId: request.tabId,
        ttlMs: ATTACHMENT_TRANSFER_GRANT_TTL_MS,
      });
      await install.promise;
      this.assertPendingLive(pending);
      if (!isExactAttachmentGrantWorker(worker, workerEpoch)) {
        throw attachmentGrantUnavailable("attachment grant worker is unavailable");
      }
      await request.authorize();
      this.assertPendingLive(pending);
      if (!isExactAttachmentGrantWorker(worker, workerEpoch)) {
        throw attachmentGrantUnavailable("attachment grant worker is unavailable");
      }
      const lease = this.installLease({
        grantId,
        ownerKey: request.ownerKey,
        deviceFingerprint: request.deviceFingerprint,
        tabId: request.tabId,
        workerFp: request.workerFp,
        workerEpoch,
        descriptor,
        expiresAtMs: this.#now() + ATTACHMENT_TRANSFER_GRANT_TTL_MS,
        workerHandle: worker,
      });
      log.info("attachment-grant-owner", "grant_installed", { worker_fp: lease.workerFp });
      return { lease: attachmentGrantSnapshot(lease), secret };
    } finally {
      if (this.#pending.get(grantId) === pending) this.#pending.delete(grantId);
      this.decrement(this.#pendingByDevice, request.deviceFingerprint);
      this.decrement(this.#pendingByOwnerTab, ownerTabKey);
      this.decrement(this.#pendingByWorker, request.workerFp);
    }
  }

  /** Removes a device's leases and revokes worker-held digest records. */
  revokeDevice(deviceFingerprint: string): number {
    this.assertOpen();
    for (const pending of this.#pending.values()) {
      if (pending.deviceFingerprint === deviceFingerprint) pending.invalidated = true;
    }
    let dropped = 0;
    for (const lease of [...this.#leases.values()]) {
      if (lease.deviceFingerprint !== deviceFingerprint) continue;
      this.dropLease(lease, "device_revoked");
      dropped += 1;
    }
    let notified = 0;
    for (const worker of connectWorkers.values()) {
      if (sendLocalAttachmentGrantRevoke(worker, deviceFingerprint)) notified += 1;
    }
    log.info("attachment-grant-owner", "device_revoked", {
      leases_dropped: dropped,
      workers_notified: notified,
    });
    return dropped;
  }

  /** Drops known attachment grants after composition sends direct retirement. */
  retireWorker(workerFp: string, reason: AttachmentGrantRetireReason): void {
    this.assertOpen();
    if (reason !== "worker_deleted" && reason !== "worker_revoked") {
      throw new Error("invalid attachment grant retirement reason");
    }
    for (const pending of this.#pending.values()) {
      if (pending.workerFp === workerFp) pending.invalidated = true;
    }
    const worker = currentRoutableWorker(workerFp);
    let dropped = 0;
    for (const lease of [...this.#leases.values()]) {
      if (lease.workerFp !== workerFp) continue;
      this.dropLease(lease, "worker_retired", reason);
      dropped += 1;
    }
    if (dropped === 0) {
      this.notify({
        kind: "worker_retired",
        lease: null,
        workerFp,
        workerEpoch: worker?.processEpoch ?? null,
        deviceFingerprint: null,
        reason,
      });
    }
    log.info("attachment-grant-owner", "worker_retired", {
      worker_fp: workerFp,
      leases_dropped: dropped,
      reason,
    });
  }

  subscribeInvalidation(listener: (invalidation: AttachmentGrantInvalidation) => void): () => void {
    this.assertOpen();
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  /** Expires only coordinator bookkeeping; workers independently enforce TTL. */
  sweep(nowMs: number): void {
    if (!Number.isFinite(nowMs)) throw new Error("attachment grant sweep requires a finite time");
    for (const lease of [...this.#leases.values()]) {
      if (lease.expiresAtMs > nowMs) continue;
      this.dropLease(lease, "grant_expired");
      log.info("attachment-grant-owner", "grant_expired", { worker_fp: lease.workerFp });
    }
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    for (const pending of this.#pending.values()) pending.invalidated = true;
    for (const lease of [...this.#leases.values()]) this.dropLease(lease, "disposed");
    this.#listeners.clear();
  }

  private installLease(snapshot: Omit<AttachmentGrantLeaseSnapshot, "descriptor"> & {
    readonly descriptor: AttachmentGrantLeaseSnapshot["descriptor"];
  }): AttachmentGrantLease {
    const lease: AttachmentGrantLease = { ...snapshot, timer: undefined };
    const delayMs = Math.max(1, lease.expiresAtMs - this.#now() + 1);
    lease.timer = setTimeout(() => this.sweep(this.#now()), delayMs);
    lease.timer.unref?.();
    this.#leases.set(lease.grantId, lease);
    this.increment(this.#leasesByOwnerTab, JSON.stringify([lease.ownerKey, lease.tabId]));
    this.increment(this.#leasesByWorker, lease.workerFp);
    return lease;
  }

  private dropLease(
    lease: AttachmentGrantLease,
    kind: AttachmentGrantInvalidation["kind"],
    reason: AttachmentGrantRetireReason | null = null,
  ): void {
    clearTimeout(lease.timer);
    if (this.#leases.get(lease.grantId) !== lease) return;
    this.#leases.delete(lease.grantId);
    this.decrement(this.#leasesByOwnerTab, JSON.stringify([lease.ownerKey, lease.tabId]));
    this.decrement(this.#leasesByWorker, lease.workerFp);
    this.notify({
      kind,
      lease: attachmentGrantSnapshot(lease),
      workerFp: lease.workerFp,
      workerEpoch: lease.workerEpoch,
      deviceFingerprint: lease.deviceFingerprint,
      reason,
    });
  }

  private assertOpen(): void {
    if (this.#disposed) throw attachmentGrantUnavailable("attachment grant owner is unavailable");
  }

  private assertPendingLive(pending: PendingAttachmentGrant): void {
    this.assertOpen();
    if (pending.invalidated) throw attachmentGrantUnavailable("attachment grant is unavailable");
  }

  private increment(counts: Map<string, number>, key: string): void {
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }

  private decrement(counts: Map<string, number>, key: string): void {
    const count = counts.get(key);
    if (count === undefined || count <= 1) counts.delete(key);
    else counts.set(key, count - 1);
  }

  private notify(invalidation: AttachmentGrantInvalidation): void {
    for (const listener of this.#listeners) {
      try {
        listener(invalidation);
      } catch {
        log.warn("attachment-grant-owner", "invalidation_listener_failed", { kind: invalidation.kind });
      }
    }
  }
}
