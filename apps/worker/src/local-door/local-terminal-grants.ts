// In-memory authorization for browser direct terminal carriers. The coordinator
// installs only SHA-256 digests; callers can read the current public scope but
// never a digest or secret. Expiry is active so a connected carrier cannot
// outlive authorization while the coordinator is unavailable.

import { create } from "@bufbuild/protobuf";
import { createHash } from "node:crypto";
import { verifyLocalEndpointCapability } from "@roost/host/local-endpoint";
import { log } from "@roost/observability/log";
import { DLocalTerminalGrantSchema, type DLocalTerminalGrant } from "@roost/protocol/proto/worker_transport_pb";
import { monoNowMs } from "../util/mono.ts";

const MAX_GRANTS = 256;
const MAX_SESSIONS_PER_GRANT = 256;
const MAX_ID_BYTES = 128;
const MAX_TTL_MS = 24 * 60 * 60_000;
const SHA256_HEX = /^[0-9a-f]{64}$/;

export interface LocalTerminalGrant {
	readonly grantId: string;
	readonly sessionIds: readonly string[];
	readonly deviceFingerprint: string;
	readonly tabId: string;
	readonly workerEpoch: string;
	readonly expiresAtMs: number;
}

export interface LocalTerminalCredential {
	readonly grantId: string;
	readonly secret: string;
	readonly tabId: string;
	readonly deviceFingerprint: string;
}

export type LocalTerminalGrantVerdict =
	| { readonly ok: true; readonly grant: LocalTerminalGrant }
	| { readonly ok: false; readonly reason: string };

export type LocalTerminalGrantRemovalReason = "expired" | "revoked" | "cleared" | "disposed";

export type LocalTerminalGrantChange =
	| {
		readonly kind: "installed" | "renewed";
		readonly grant: LocalTerminalGrant;
		readonly previous: LocalTerminalGrant | null;
		readonly removedSessionIds: readonly string[];
	}
	| {
		readonly kind: "removed";
		readonly grant: LocalTerminalGrant;
		readonly reason: LocalTerminalGrantRemovalReason;
	};

export interface LocalTerminalGrantStoreOptions {
	/** New direct grants are process-epoch-fenced; empty old-worker grants stay loopback-only. */
	readonly workerEpoch?: string;
	readonly now?: () => number;
	readonly scheduleTimeout?: (callback: () => void, delayMs: number) => NodeJS.Timeout;
	readonly clearTimeout?: (timer: NodeJS.Timeout) => void;
}

interface StoredGrant extends LocalTerminalGrant {
	readonly secretSha256: string;
	readonly publicScope: LocalTerminalGrant;
}

/** One worker-owned grant registry. Subscription callbacks synchronously fence
 * live ports before their next queued write can reach the keeper. */
export class LocalTerminalGrantStore {
	private readonly grants = new Map<string, StoredGrant>();
	private readonly expiryTimers = new Map<string, NodeJS.Timeout>();
	private readonly listeners = new Set<(change: LocalTerminalGrantChange) => void>();
	private readonly expiredGrantIds = new Set<string>();
	private readonly now: () => number;
	private readonly workerEpoch: string | undefined;
	private readonly scheduleTimeout: (callback: () => void, delayMs: number) => NodeJS.Timeout;
	private readonly clearScheduledTimeout: (timer: NodeJS.Timeout) => void;
	private disposed = false;

	constructor(options: LocalTerminalGrantStoreOptions | (() => number) = {}) {
		const normalized = typeof options === "function" ? { now: options } : options;
		this.now = normalized.now ?? monoNowMs;
		this.workerEpoch = normalized.workerEpoch;
		this.scheduleTimeout = normalized.scheduleTimeout ?? setTimeout;
		this.clearScheduledTimeout = normalized.clearTimeout ?? clearTimeout;
	}

	subscribe(listener: (change: LocalTerminalGrantChange) => void): () => void {
		this.listeners.add(listener);
		return () => { this.listeners.delete(listener); };
	}

	/** Install or renew one coordinator-authorized scope. The returned object
	 * deliberately has no credential digest. */
	install(frame: DLocalTerminalGrant): LocalTerminalGrant {
		if (this.disposed) throw new Error("local terminal grant store is disposed");
		const sessionIds = validSessionIds(frame.sessionIds);
		if (!validId(frame.grantId)) throw new Error("grant_id is invalid");
		if (!SHA256_HEX.test(frame.secretSha256)) {
			throw new Error("secret_sha256 must be a lowercase hex SHA-256 digest");
		}
		if (!sessionIds) throw new Error("session_ids is invalid");
		if (!validId(frame.deviceFingerprint)) throw new Error("device_fingerprint is invalid");
		if (!validId(frame.tabId)) throw new Error("tab_id is invalid");
		if (frame.workerEpoch !== "" && !validId(frame.workerEpoch)) throw new Error("worker_epoch is invalid");
		if (this.workerEpoch && frame.workerEpoch !== "" && frame.workerEpoch !== this.workerEpoch) {
			throw new Error("worker_epoch does not match this worker");
		}
		if (!Number.isInteger(frame.ttlMs) || frame.ttlMs <= 0 || frame.ttlMs > MAX_TTL_MS) {
			throw new Error(`ttl_ms must be within 1..${MAX_TTL_MS}`);
		}
		this.sweep();
		const prior = this.grants.get(frame.grantId);
		if (!prior && this.grants.size >= MAX_GRANTS) {
			throw new Error("local terminal grant capacity is full");
		}
		const publicScope = immutablePublicGrant({
			grantId: frame.grantId,
			sessionIds,
			deviceFingerprint: frame.deviceFingerprint,
			tabId: frame.tabId,
			workerEpoch: frame.workerEpoch,
			expiresAtMs: this.now() + frame.ttlMs,
		});
		const grant: StoredGrant = {
			...publicScope,
			secretSha256: frame.secretSha256,
			publicScope,
		};
		this.clearExpiryTimer(grant.grantId);
		this.grants.set(grant.grantId, grant);
		this.expiredGrantIds.delete(grant.grantId);
		this.armExpiry(grant);
		const previous = prior?.publicScope ?? null;
		const removedSessionIds = previous
			? previous.sessionIds.filter((sessionId) => !publicScope.sessionIds.includes(sessionId))
			: [];
		this.notify({
			kind: previous ? "renewed" : "installed",
			grant: publicScope,
			previous,
			removedSessionIds,
		});
		log.info("local-terminal", "grant_installed", {
			grant_id: grant.grantId,
			device_fingerprint: grant.deviceFingerprint,
			tab_id: grant.tabId,
			sessions: sessionIds.length,
			ttl_ms: frame.ttlMs,
			renewed: previous !== null,
		});
		return publicScope;
	}

	/** Current public scope only. A live predicate must call this at its final
	 * keeper boundary rather than retaining an old allow-list. */
	current(grantId: string): LocalTerminalGrant | null {
		return this.currentStored(grantId)?.publicScope ?? null;
	}

	verify(credential: LocalTerminalCredential): LocalTerminalGrantVerdict {
		const grant = this.currentStored(credential.grantId);
		if (!grant) {
			return {
				ok: false,
				reason: this.expiredGrantIds.has(credential.grantId)
					? "local terminal grant expired"
					: "unknown local terminal grant",
			};
		}
		if (grant.deviceFingerprint !== credential.deviceFingerprint) {
			return { ok: false, reason: "grant is bound to another device" };
		}
		if (grant.tabId !== credential.tabId) {
			return { ok: false, reason: "grant is bound to another tab" };
		}
		const received = createHash("sha256").update(credential.secret).digest("hex");
		if (!verifyLocalEndpointCapability(grant.secretSha256, received)) {
			return { ok: false, reason: "local terminal grant secret mismatch" };
		}
		return { ok: true, grant: grant.publicScope };
	}

	/** Peer offers have no secret; the authenticated tuple is checked against
	 * the live grant scope and an exact worker epoch. */
	authorizePeer(
		grantId: string,
		deviceFingerprint: string,
		tabId: string,
		workerEpoch: string,
	): "authorized" | "grant_unavailable" | "expired" {
		const stored = this.grants.get(grantId);
		if (!stored) return this.expiredGrantIds.has(grantId) ? "expired" : "grant_unavailable";
		if (stored.expiresAtMs <= this.now()) {
			this.remove(grantId, "expired");
			return "expired";
		}
		if (!this.workerEpoch || workerEpoch !== this.workerEpoch || stored.workerEpoch !== this.workerEpoch) {
			return "grant_unavailable";
		}
		return stored.deviceFingerprint === deviceFingerprint && stored.tabId === tabId
			? "authorized"
			: "grant_unavailable";
	}

	revokeDevice(deviceFingerprint: string): readonly string[] {
		const revoked: string[] = [];
		for (const grant of [...this.grants.values()]) {
			if (grant.deviceFingerprint !== deviceFingerprint) continue;
			this.remove(grant.grantId, "revoked");
			revoked.push(grant.grantId);
		}
		if (revoked.length > 0) {
			log.info("local-terminal", "grants_revoked", {
				device_fingerprint: deviceFingerprint,
				grants: revoked.length,
			});
		}
		return revoked;
	}

	remove(grantId: string, reason: LocalTerminalGrantRemovalReason = "cleared"): boolean {
		const grant = this.grants.get(grantId);
		if (!grant) return false;
		this.grants.delete(grantId);
		this.clearExpiryTimer(grantId);
		if (reason === "expired") this.rememberExpired(grantId);
		this.notify({ kind: "removed", grant: grant.publicScope, reason });
		return true;
	}

	count(): number {
		this.sweep();
		return this.grants.size;
	}

	/** In-process smoke seam: an injected monotonic clock advances, then expiry fences live ports. */
	_sweepExpiredForTest(): void {
		this.sweep();
	}

	/** In-process smoke seam: renew matching live grants without one removed session. */
	_shrinkSessionForTest(sessionId: string): number {
		let changed = 0;
		for (const grant of [...this.grants.values()]) {
			if (!grant.sessionIds.includes(sessionId)) continue;
			const remainingSessionIds = grant.sessionIds.filter((candidate) => candidate !== sessionId);
			if (remainingSessionIds.length === 0 || grant.expiresAtMs <= this.now()) {
				this.remove(grant.grantId, grant.expiresAtMs <= this.now() ? "expired" : "cleared");
				changed += 1;
				continue;
			}
			const remainingTtlMs = Math.max(1, Math.ceil(grant.expiresAtMs - this.now()));
			this.install(create(DLocalTerminalGrantSchema, {
				requestId: grant.grantId,
				grantId: grant.grantId,
				secretSha256: grant.secretSha256,
				sessionIds: remainingSessionIds,
				deviceFingerprint: grant.deviceFingerprint,
				tabId: grant.tabId,
				ttlMs: remainingTtlMs,
				workerEpoch: grant.workerEpoch,
			}));
			changed += 1;
		}
		return changed;
	}

	clear(): void {
		for (const grantId of [...this.grants.keys()]) this.remove(grantId, "cleared");
	}

	dispose(): void {
		if (this.disposed) return;
		this.disposed = true;
		for (const grantId of [...this.grants.keys()]) this.remove(grantId, "disposed");
		for (const timer of this.expiryTimers.values()) this.clearScheduledTimeout(timer);
		this.expiryTimers.clear();
		this.listeners.clear();
	}

	private currentStored(grantId: string): StoredGrant | null {
		const grant = this.grants.get(grantId);
		if (!grant) return null;
		if (grant.expiresAtMs > this.now()) return grant;
		this.remove(grantId, "expired");
		return null;
	}

	private armExpiry(grant: StoredGrant): void {
		const delayMs = Math.max(0, grant.expiresAtMs - this.now());
		const timer = this.scheduleTimeout(() => {
			this.expiryTimers.delete(grant.grantId);
			const current = this.grants.get(grant.grantId);
			if (!current || current !== grant) return;
			if (current.expiresAtMs > this.now()) {
				this.armExpiry(current);
				return;
			}
			this.remove(current.grantId, "expired");
		}, delayMs);
		timer.unref?.();
		this.expiryTimers.set(grant.grantId, timer);
	}

	private clearExpiryTimer(grantId: string): void {
		const timer = this.expiryTimers.get(grantId);
		if (!timer) return;
		this.expiryTimers.delete(grantId);
		this.clearScheduledTimeout(timer);
	}

	private sweep(): void {
		for (const grant of [...this.grants.values()]) {
			if (grant.expiresAtMs <= this.now()) this.remove(grant.grantId, "expired");
		}
	}

	private rememberExpired(grantId: string): void {
		if (this.expiredGrantIds.has(grantId)) return;
		if (this.expiredGrantIds.size >= MAX_GRANTS) {
			const oldest = this.expiredGrantIds.values().next().value;
			if (oldest) this.expiredGrantIds.delete(oldest);
		}
		this.expiredGrantIds.add(grantId);
	}

	private notify(change: LocalTerminalGrantChange): void {
		for (const listener of [...this.listeners]) listener(change);
	}
}

function immutablePublicGrant(grant: LocalTerminalGrant): LocalTerminalGrant {
	return Object.freeze({
		grantId: grant.grantId,
		sessionIds: Object.freeze([...grant.sessionIds]),
		deviceFingerprint: grant.deviceFingerprint,
		tabId: grant.tabId,
		workerEpoch: grant.workerEpoch,
		expiresAtMs: grant.expiresAtMs,
	});
}

function validId(value: string): boolean {
	const byteLength = Buffer.byteLength(value, "utf8");
	return byteLength > 0 && byteLength <= MAX_ID_BYTES;
}

function validSessionIds(values: readonly string[]): readonly string[] | null {
	if (values.length === 0 || values.length > MAX_SESSIONS_PER_GRANT) return null;
	const seen = new Set<string>();
	const unique: string[] = [];
	for (const value of values) {
		if (!validId(value)) return null;
		if (seen.has(value)) continue;
		seen.add(value);
		unique.push(value);
	}
	return unique;
}
