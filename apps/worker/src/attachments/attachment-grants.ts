// In-memory authority for one exact direct attachment upload.
// Coordinator installs only a secret digest; this store binds the hello's
// device, tab, worker epoch, and immutable upload descriptor independently.

import { createHash } from "node:crypto";
import { verifyLocalEndpointCapability } from "@roost/host/local-endpoint";
import { log } from "@roost/observability/log";
import { ATTACHMENT_TRANSFER_GRANT_TTL_MS } from "@roost/protocol/attachment-transfer";
import {
  type DLocalAttachmentGrant,
} from "@roost/protocol/proto/worker_transport_pb";
import { monoNowMs } from "../util/mono.ts";

const MAX_GRANTS = 256;
const MAX_ID_BYTES = 128;
const MAX_FILENAME_BYTES = 255;
const MAX_TTL_MS = ATTACHMENT_TRANSFER_GRANT_TTL_MS;
const SHA256_HEX = /^[0-9a-f]{64}$/;

export interface AttachmentGrant {
  readonly grantId: string;
  readonly sessionId: string;
  readonly uploadId: string;
  readonly filename: string;
  readonly shortPath: boolean;
  readonly totalBytes: number;
  readonly deviceFingerprint: string;
  readonly tabId: string;
  readonly workerEpoch: string;
  readonly expiresAtMs: number;
}

export interface AttachmentGrantCredential {
  readonly grantId: string;
  readonly secret: string;
  readonly sessionId: string;
  readonly uploadId: string;
  readonly filename: string;
  readonly shortPath: boolean;
  readonly totalBytes: bigint;
  readonly deviceFingerprint: string;
  readonly tabId: string;
  readonly workerEpoch: string;
}

export type AttachmentGrantVerdict =
  | { readonly ok: true; readonly grant: AttachmentGrant }
  | { readonly ok: false; readonly reason: string };

export type AttachmentPeerGrantAuthorization = "authorized" | "grant_unavailable" | "expired";
export type AttachmentGrantRemovalReason = "expired" | "revoked" | "cleared" | "disposed";

export type AttachmentGrantChange =
  | {
      readonly kind: "installed";
      readonly grant: AttachmentGrant;
      readonly previous: AttachmentGrant | null;
    }
  | {
      readonly kind: "removed";
      readonly grant: AttachmentGrant;
      readonly reason: AttachmentGrantRemovalReason;
    };


export interface AttachmentGrantStoreOptions {
  readonly workerEpoch: string;
  readonly now?: () => number;
  readonly scheduleTimeout?: (callback: () => void, delayMs: number) => NodeJS.Timeout;
  readonly clearTimeout?: (timer: NodeJS.Timeout) => void;
}

interface StoredAttachmentGrant extends AttachmentGrant {
  readonly secretSha256: string;
  readonly publicGrant: AttachmentGrant;
}

/** Separate process-owned registry; terminal grants never enter this state. */
export class AttachmentGrantStore {
  private readonly grants = new Map<string, StoredAttachmentGrant>();
  private readonly expiryTimers = new Map<string, NodeJS.Timeout>();
  private readonly listeners = new Set<(change: AttachmentGrantChange) => void>();
  private readonly now: () => number;
  private readonly scheduleTimeout: (callback: () => void, delayMs: number) => NodeJS.Timeout;
  private readonly clearScheduledTimeout: (timer: NodeJS.Timeout) => void;
  private disposed = false;

  constructor(private readonly options: AttachmentGrantStoreOptions) {
    this.now = options.now ?? monoNowMs;
    this.scheduleTimeout = options.scheduleTimeout ?? setTimeout;
    this.clearScheduledTimeout = options.clearTimeout ?? clearTimeout;
  }

  subscribe(listener: (change: AttachmentGrantChange) => void): () => void {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  }

  install(frame: DLocalAttachmentGrant): AttachmentGrant {
    if (this.disposed) throw new Error("attachment grant store is disposed");
    if (!validGrantFrame(frame, this.options.workerEpoch)) throw new Error("attachment grant is invalid");
    this.sweepExpired();
    const prior = this.grants.get(frame.grantId);
    if (!prior && this.grants.size >= MAX_GRANTS) throw new Error("attachment grant capacity is full");
    const publicGrant = immutableGrant({
      grantId: frame.grantId,
      sessionId: frame.sessionId,
      uploadId: frame.uploadId,
      filename: frame.filename,
      shortPath: frame.shortPath,
      totalBytes: Number(frame.totalBytes),
      deviceFingerprint: frame.deviceFingerprint,
      tabId: frame.tabId,
      workerEpoch: frame.workerEpoch,
      expiresAtMs: this.now() + frame.ttlMs,
    });
    this.clearExpiryTimer(frame.grantId);
    this.grants.set(frame.grantId, {
      ...publicGrant,
      secretSha256: frame.secretSha256,
      publicGrant,
    });
    this.armExpiry(frame.grantId, publicGrant.expiresAtMs);
    this.notify({
      kind: "installed",
      grant: publicGrant,
      previous: prior?.publicGrant ?? null,
    });
    log.info("attachment-transfer", "grant_installed", {
      ttl_ms: frame.ttlMs,
      renewed: prior !== undefined,
    });
    return publicGrant;
  }

  current(grantId: string): AttachmentGrant | null {
    return this.currentStored(grantId)?.publicGrant ?? null;
  }

  verify(credential: AttachmentGrantCredential): AttachmentGrantVerdict {
    const grant = this.currentStored(credential.grantId);
    if (!grant) return { ok: false, reason: "attachment grant is unavailable" };
    if (credential.totalBytes > BigInt(Number.MAX_SAFE_INTEGER)) {
      return { ok: false, reason: "attachment grant size is invalid" };
    }
    if (!sameDescriptor(grant, credential) || grant.deviceFingerprint !== credential.deviceFingerprint || grant.tabId !== credential.tabId) {
      return { ok: false, reason: "attachment grant does not match upload" };
    }
    const received = createHash("sha256").update(credential.secret).digest("hex");
    if (!verifyLocalEndpointCapability(grant.secretSha256, received)) {
      return { ok: false, reason: "attachment grant secret mismatch" };
    }
    return { ok: true, grant: grant.publicGrant };
  }

  authorizePeer(request: {
    readonly grantId: string;
    readonly deviceFingerprint: string;
    readonly tabId: string;
    readonly workerEpoch: string;
  }): AttachmentPeerGrantAuthorization {
    const stored = this.grants.get(request.grantId);
    if (!stored) return "grant_unavailable";
    if (stored.expiresAtMs <= this.now()) {
      this.remove(request.grantId, "expired");
      return "expired";
    }
    if (request.workerEpoch !== this.options.workerEpoch || stored.workerEpoch !== this.options.workerEpoch) {
      return "grant_unavailable";
    }
    return stored.deviceFingerprint === request.deviceFingerprint && stored.tabId === request.tabId
      ? "authorized"
      : "grant_unavailable";
  }

  revokeDevice(deviceFingerprint: string): number {
    let revoked = 0;
    for (const grant of [...this.grants.values()]) {
      if (grant.deviceFingerprint !== deviceFingerprint) continue;
      this.remove(grant.grantId, "revoked");
      revoked += 1;
    }
    if (revoked > 0) log.info("attachment-transfer", "grants_revoked", { grants: revoked });
    return revoked;
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    for (const grantId of [...this.grants.keys()]) this.remove(grantId, "disposed");
    for (const timer of this.expiryTimers.values()) this.clearScheduledTimeout(timer);
    this.expiryTimers.clear();
    this.listeners.clear();
  }

  private currentStored(grantId: string): StoredAttachmentGrant | null {
    const grant = this.grants.get(grantId);
    if (!grant) return null;
    if (grant.expiresAtMs > this.now()) return grant;
    this.remove(grantId, "expired");
    return null;
  }

  private armExpiry(grantId: string, expiresAtMs: number): void {
    const timer = this.scheduleTimeout(() => {
      this.expiryTimers.delete(grantId);
      const current = this.grants.get(grantId);
      if (!current) return;
      if (current.expiresAtMs > this.now()) {
        this.armExpiry(grantId, current.expiresAtMs);
        return;
      }
      this.remove(grantId, "expired");
    }, Math.max(0, expiresAtMs - this.now()));
    timer.unref?.();
    this.expiryTimers.set(grantId, timer);
  }

  private clearExpiryTimer(grantId: string): void {
    const timer = this.expiryTimers.get(grantId);
    if (!timer) return;
    this.expiryTimers.delete(grantId);
    this.clearScheduledTimeout(timer);
  }

  private remove(grantId: string, reason: AttachmentGrantRemovalReason = "cleared"): void {
    const grant = this.grants.get(grantId);
    if (!grant) return;
    this.grants.delete(grantId);
    this.clearExpiryTimer(grantId);
    this.notify({ kind: "removed", grant: grant.publicGrant, reason });
  }

  private sweepExpired(): void {
    for (const grant of [...this.grants.values()]) {
      if (grant.expiresAtMs <= this.now()) this.remove(grant.grantId, "expired");
    }
  }

  private notify(change: AttachmentGrantChange): void {
    for (const listener of [...this.listeners]) listener(change);
  }
}

function validGrantFrame(frame: DLocalAttachmentGrant, workerEpoch: string): boolean {
  return validOpaqueId(frame.requestId)
    && validOpaqueId(frame.grantId)
    && SHA256_HEX.test(frame.secretSha256)
    && validOpaqueId(frame.sessionId)
    && validOpaqueId(frame.uploadId)
    && validFilename(frame.filename)
    && Number.isSafeInteger(frame.ttlMs)
    && frame.ttlMs > 0
    && frame.ttlMs <= MAX_TTL_MS
    && frame.workerEpoch === workerEpoch
    && frame.totalBytes <= BigInt(Number.MAX_SAFE_INTEGER)
    && validOpaqueId(frame.deviceFingerprint)
    && validOpaqueId(frame.tabId);
}

function validOpaqueId(value: string): boolean {
  const bytes = Buffer.byteLength(value, "utf8");
  return bytes > 0 && bytes <= MAX_ID_BYTES && !value.includes("/") && !value.includes("\\") && !/[\x00-\x1f\x7f]/.test(value);
}

function validFilename(value: string): boolean {
  const bytes = Buffer.byteLength(value, "utf8");
  return bytes > 0 && bytes <= MAX_FILENAME_BYTES && !value.includes("\0");
}

function sameDescriptor(grant: StoredAttachmentGrant, credential: AttachmentGrantCredential): boolean {
  return grant.sessionId === credential.sessionId
    && grant.uploadId === credential.uploadId
    && grant.filename === credential.filename
    && grant.shortPath === credential.shortPath
    && grant.totalBytes === Number(credential.totalBytes)
    && grant.workerEpoch === credential.workerEpoch;
}

function immutableGrant(grant: AttachmentGrant): AttachmentGrant {
  return Object.freeze({ ...grant });
}
