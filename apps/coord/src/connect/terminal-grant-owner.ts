// Composition-owned direct-terminal grant leases. The session grant handler mints
// browser secrets through this owner, while peer negotiation reads exact live leases.
// One bounded refresh per owner/tab/worker coalesces demand without retaining a
// secret or digest after the worker acknowledges installation.

import { createHash, randomBytes, randomUUID } from "node:crypto";
import { Code, ConnectError } from "@connectrpc/connect";
import { log } from "@roost/shared/log";
import { TERMINAL_PEER_MAX_SESSIONS_PER_GRANT } from "@roost/shared/terminal-peer";
import type { WorkerHandle } from "./worker-registry.ts";
import { connectWorkers } from "./worker-registry.ts";
import { currentRoutableWorker } from "./worker-send-target.ts";
import {
  grantCapacityExceeded,
  invalidGrantSessions,
  isExactTerminalGrantWorker,
  terminalGrantLeaseKey,
  terminalGrantSnapshot,
  workerUnavailable,
  type PendingGrantRefresh,
  type TerminalGrantInvalidation,
  type TerminalGrantInvalidationKind,
  type TerminalGrantLease,
} from "./terminal-grant-owner-state.ts";
import {
  sendLocalTerminalGrantRequest,
  sendLocalTerminalGrantRevoke,
  sendTerminalDirectRetire,
} from "./worker-send-local-terminal.ts";
export const LOCAL_TERMINAL_GRANT_TTL_MS = 12 * 60 * 60_000;
const MAX_PENDING_TERMINAL_GRANT_REFRESHES = 256;
export type TerminalDirectRetireReason = "worker_deleted" | "worker_revoked";
export type { TerminalGrantInvalidation, TerminalGrantInvalidationKind } from "./terminal-grant-owner-state.ts";

/** The non-secret lease data that signaling may inspect after authenticating a caller. */
export interface TerminalGrantLeaseSnapshot {
  readonly grantId: string;
  readonly ownerKey: string;
  readonly deviceFingerprint: string;
  readonly tabId: string;
  readonly workerFp: string;
  readonly workerEpoch: string | null;
  readonly sessionIds: readonly string[];
  readonly expiresAtMs: number;
  /** The exact current worker connection allowed to carry this grant. */
  readonly workerHandle: WorkerHandle;
}
export type TerminalGrantAuthorization = (sessionIds: readonly string[]) => Promise<void>;
export interface TerminalGrantRequest {
  readonly ownerKey: string;
  readonly deviceFingerprint: string;
  readonly tabId: string;
  readonly workerFp: string;
  readonly sessionIds: readonly string[];
  /** Re-runs durable route authority for the union this refresh will install. */
  readonly authorize: TerminalGrantAuthorization;
}

export interface TerminalGrantResult { readonly lease: TerminalGrantLeaseSnapshot; readonly secret: string; }
export interface TerminalGrantOwnerOptions { readonly now?: () => number; }

/** One composition-owned registry for all direct terminal grants in a coordinator. */
export class TerminalGrantOwner {
  readonly #leases = new Map<string, TerminalGrantLease>();
  readonly #listeners = new Set<(invalidation: TerminalGrantInvalidation) => void>();
  readonly #refreshes = new Map<string, PendingGrantRefresh>();
  readonly #now: () => number;
  #disposed = false;

  constructor(options: TerminalGrantOwnerOptions = {}) {
    this.#now = options.now ?? Date.now;
  }

  /** Returns only a still-owned lease on the exact worker generation that installed it. */
  ownedGrant(
    ownerKey: string,
    tabId: string,
    workerFp: string,
    grantId: string,
  ): TerminalGrantLeaseSnapshot | null {
    this.sweep(this.#now());
    const key = terminalGrantLeaseKey(ownerKey, tabId, workerFp);
    let lease = this.#leases.get(key);
    const worker = currentRoutableWorker(workerFp);
    if (!lease || lease.grantId !== grantId || !worker || worker.processEpoch !== lease.workerEpoch) {
      return null;
    }
    if (lease.workerHandle !== worker) {
      lease = { ...lease, workerHandle: worker };
      this.#leases.set(key, lease);
      log.info("terminal-grant-owner", "grant_rebound", { worker_fp: workerFp });
    }
    return terminalGrantSnapshot(lease);
  }

  /** Coalesces one tuple's demand and returns the final credential covering its stable union. */
  grant(request: TerminalGrantRequest): Promise<TerminalGrantResult> {
    this.assertOpen();
    this.sweep(this.#now());
    if (request.sessionIds.length === 0 || request.sessionIds.length > TERMINAL_PEER_MAX_SESSIONS_PER_GRANT) {
      return Promise.reject(invalidGrantSessions());
    }
    const key = terminalGrantLeaseKey(request.ownerKey, request.tabId, request.workerFp);
    const existing = this.#refreshes.get(key);
    if (existing) {
      if (existing.invalidated || existing.deviceFingerprint !== request.deviceFingerprint) {
        return Promise.reject(workerUnavailable());
      }
      if (!this.mergeDemand(existing, request.sessionIds)) return Promise.reject(grantCapacityExceeded());
      existing.authorize = request.authorize;
      return existing.promise;
    }
    if (this.#refreshes.size >= MAX_PENDING_TERMINAL_GRANT_REFRESHES) {
      return Promise.reject(grantCapacityExceeded());
    }
    const deferred = Promise.withResolvers<TerminalGrantResult>();
    const refresh: PendingGrantRefresh = {
      key,
      ownerKey: request.ownerKey,
      deviceFingerprint: request.deviceFingerprint,
      tabId: request.tabId,
      workerFp: request.workerFp,
      sessionIds: new Set(),
      authorize: request.authorize,
      invalidated: false,
      promise: deferred.promise,
      resolve: deferred.resolve,
      reject: deferred.reject,
    };
    if (!this.mergeDemand(refresh, request.sessionIds)) return Promise.reject(grantCapacityExceeded());
    this.#refreshes.set(key, refresh);
    void Promise.resolve()
      .then(() => this.runRefresh(refresh))
      .finally(() => {
        if (this.#refreshes.get(key) === refresh) this.#refreshes.delete(key);
      })
      .then(refresh.resolve, refresh.reject);
    return refresh.promise;
  }

  /** Drops all coordinator lease rows for a device and tells every currently routable worker. */
  revokeDevice(deviceFingerprint: string): number {
    this.assertOpen();
    for (const refresh of this.#refreshes.values()) {
      if (refresh.deviceFingerprint === deviceFingerprint) refresh.invalidated = true;
    }
    let dropped = 0;
    for (const [key, lease] of [...this.#leases]) {
      if (lease.deviceFingerprint !== deviceFingerprint) continue;
      this.dropLease(key, lease, "device_revoked");
      dropped += 1;
    }
    let notified = 0;
    for (const worker of connectWorkers.values()) {
      if (!worker.ready || worker.revoked) continue;
      if (sendLocalTerminalGrantRevoke(worker, deviceFingerprint)) notified += 1;
    }
    log.info("terminal-grant-owner", "device_revoked", {
      leases_dropped: dropped,
      workers_notified: notified,
    });
    return dropped;
  }

  /** Sends retirement while the captured worker handle is still live, then invalidates its leases. */
  retireWorker(workerFp: string, reason: TerminalDirectRetireReason): void {
    this.assertOpen();
    if (reason !== "worker_deleted" && reason !== "worker_revoked") {
      throw new Error("invalid terminal direct retirement reason");
    }
    const worker = currentRoutableWorker(workerFp);
    const workerEpoch = worker?.processEpoch ?? null;
    if (worker && workerEpoch !== null) sendTerminalDirectRetire(worker, workerEpoch, reason);
    for (const refresh of this.#refreshes.values()) {
      if (refresh.workerFp === workerFp) refresh.invalidated = true;
    }
    let dropped = 0;
    for (const [key, lease] of [...this.#leases]) {
      if (lease.workerFp !== workerFp) continue;
      this.dropLease(key, lease, "worker_retired", reason);
      dropped += 1;
    }
    if (dropped === 0) {
      this.notify({
        kind: "worker_retired",
        lease: null,
        workerFp,
        workerEpoch,
        deviceFingerprint: null,
        removedSessionIds: [],
        reason,
      });
    }
    log.info("terminal-grant-owner", "worker_retired", {
      worker_fp: workerFp,
      leases_dropped: dropped,
      reason,
    });
  }

  subscribeInvalidation(listener: (invalidation: TerminalGrantInvalidation) => void): () => void {
    this.assertOpen();
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  /** Snapshot for diagnostics and focused tests; it never exposes a secret or digest. */
  list(): TerminalGrantLeaseSnapshot[] {
    this.sweep(this.#now());
    return [...this.#leases.values()].map(terminalGrantSnapshot);
  }

  /** Expires only coordinator bookkeeping; workers independently enforce their grant TTL. */
  sweep(nowMs: number): void {
    if (!Number.isFinite(nowMs)) throw new Error("terminal grant sweep requires a finite time");
    for (const [key, lease] of [...this.#leases]) {
      if (lease.expiresAtMs > nowMs) continue;
      this.dropLease(key, lease, "grant_expired");
      log.info("terminal-grant-owner", "grant_expired", { worker_fp: lease.workerFp });
    }
  }

  /** Stops timers and makes every in-flight refresh fail its post-ACK identity check. */
  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    for (const refresh of this.#refreshes.values()) refresh.invalidated = true;
    for (const [key, lease] of [...this.#leases]) this.dropLease(key, lease, "disposed");
    this.#listeners.clear();
  }

  private async runRefresh(refresh: PendingGrantRefresh): Promise<TerminalGrantResult> {
    for (;;) {
      this.assertRefreshLive(refresh);
      const sessionIds = Object.freeze([...refresh.sessionIds]);
      const authorize = refresh.authorize;
      const result = await this.issueGrant(refresh, sessionIds, authorize);
      this.assertRefreshLive(refresh);
      if (
        sessionIds.length === refresh.sessionIds.size
        && sessionIds.every((sessionId) => refresh.sessionIds.has(sessionId))
      ) {
        if (this.#refreshes.get(refresh.key) === refresh) this.#refreshes.delete(refresh.key);
        return result;
      }
    }
  }

  private async issueGrant(
    refresh: PendingGrantRefresh,
    sessionIds: readonly string[],
    authorize: TerminalGrantAuthorization,
  ): Promise<TerminalGrantResult> {
    this.assertRefreshLive(refresh);
    await authorize(sessionIds);
    this.assertRefreshLive(refresh);
    const worker = currentRoutableWorker(refresh.workerFp);
    if (!worker) throw workerUnavailable();
    const workerEpoch = worker.processEpoch;
    if (!isExactTerminalGrantWorker(worker, workerEpoch)) throw workerUnavailable();

    const existing = this.#leases.get(refresh.key);
    const renewedLease = existing?.workerEpoch === workerEpoch ? existing : undefined;
    const grantId = renewedLease?.grantId ?? randomUUID();
    const secret = randomBytes(32).toString("hex");
    const install = sendLocalTerminalGrantRequest(worker, workerEpoch, {
      grantId,
      secretSha256: createHash("sha256").update(secret).digest("hex"),
      sessionIds,
      deviceFingerprint: refresh.deviceFingerprint,
      tabId: refresh.tabId,
      ttlMs: LOCAL_TERMINAL_GRANT_TTL_MS,
    });
    await install.promise;

    this.assertRefreshLive(refresh);
    if (!isExactTerminalGrantWorker(worker, workerEpoch)) throw workerUnavailable();
    await authorize(sessionIds);
    this.assertRefreshLive(refresh);
    if (!isExactTerminalGrantWorker(worker, workerEpoch)) throw workerUnavailable();

    const nowMs = this.#now();
    const lease = this.installLease({
      grantId,
      ownerKey: refresh.ownerKey,
      deviceFingerprint: refresh.deviceFingerprint,
      tabId: refresh.tabId,
      workerFp: refresh.workerFp,
      workerEpoch,
      sessionIds,
      expiresAtMs: nowMs + LOCAL_TERMINAL_GRANT_TTL_MS,
      workerHandle: worker,
    });
    if (existing && !renewedLease) {
      this.notify({
        kind: "grant_replaced",
        lease: terminalGrantSnapshot(existing),
        workerFp: existing.workerFp,
        workerEpoch: existing.workerEpoch,
        deviceFingerprint: existing.deviceFingerprint,
        removedSessionIds: existing.sessionIds,
        reason: null,
      });
    }
    if (renewedLease) {
      const allowed = new Set(lease.sessionIds);
      const removedSessionIds = renewedLease.sessionIds.filter((sessionId) => !allowed.has(sessionId));
      if (removedSessionIds.length > 0) {
        this.notify({
          kind: "scope_reduced",
          lease: terminalGrantSnapshot(lease),
          workerFp: lease.workerFp,
          workerEpoch: lease.workerEpoch,
          deviceFingerprint: lease.deviceFingerprint,
          removedSessionIds,
          reason: null,
        });
      }
      log.info("terminal-grant-owner", "grant_renewed", {
        worker_fp: lease.workerFp,
        sessions: lease.sessionIds.length,
      });
    } else {
      log.info("terminal-grant-owner", "grant_installed", {
        worker_fp: lease.workerFp,
        sessions: lease.sessionIds.length,
      });
    }
    return { lease: terminalGrantSnapshot(lease), secret };
  }

  private mergeDemand(refresh: PendingGrantRefresh, sessionIds: readonly string[]): boolean {
    const additions = new Set<string>();
    for (const sessionId of sessionIds) {
      if (!refresh.sessionIds.has(sessionId)) additions.add(sessionId);
    }
    if (refresh.sessionIds.size + additions.size > TERMINAL_PEER_MAX_SESSIONS_PER_GRANT) return false;
    for (const sessionId of additions) refresh.sessionIds.add(sessionId);
    return true;
  }

  private installLease(snapshot: Omit<TerminalGrantLeaseSnapshot, "sessionIds"> & {
    readonly sessionIds: readonly string[];
  }): TerminalGrantLease {
    const key = terminalGrantLeaseKey(snapshot.ownerKey, snapshot.tabId, snapshot.workerFp);
    const previous = this.#leases.get(key);
    clearTimeout(previous?.timer);
    const lease: TerminalGrantLease = {
      ...snapshot,
      sessionIds: Object.freeze([...snapshot.sessionIds]),
      timer: undefined,
    };
    const delayMs = Math.max(1, lease.expiresAtMs - this.#now() + 1);
    lease.timer = setTimeout(() => this.sweep(this.#now()), delayMs);
    lease.timer.unref?.();
    this.#leases.set(key, lease);
    return lease;
  }

  private dropLease(
    key: string,
    lease: TerminalGrantLease,
    kind: Extract<TerminalGrantInvalidationKind, "grant_expired" | "device_revoked" | "worker_retired" | "disposed">,
    reason: TerminalDirectRetireReason | null = null,
  ): void {
    clearTimeout(lease.timer);
    if (this.#leases.get(key) === lease) this.#leases.delete(key);
    this.notify({
      kind,
      lease: terminalGrantSnapshot(lease),
      workerFp: lease.workerFp,
      workerEpoch: lease.workerEpoch,
      deviceFingerprint: lease.deviceFingerprint,
      removedSessionIds: lease.sessionIds,
      reason,
    });
  }

  private assertOpen(): void {
    if (this.#disposed) throw new ConnectError("terminal grant owner disposed", Code.Unavailable);
  }

  private assertRefreshLive(refresh: PendingGrantRefresh): void {
    this.assertOpen();
    if (refresh.invalidated) throw workerUnavailable();
  }

  private notify(invalidation: TerminalGrantInvalidation): void {
    for (const listener of this.#listeners) {
      try {
        listener(invalidation);
      } catch {
        log.warn("terminal-grant-owner", "invalidation_listener_failed", { kind: invalidation.kind });
      }
    }
  }
}


