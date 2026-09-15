// In-memory authorization for a browser on this machine to reach the worker's
// loopback terminal socket. The coordinator mints the secret, installs only its
// SHA-256 digest here, and revokes per device; local-terminal-socket.ts checks
// every hello through this store. Nothing is persisted: a worker restart
// deliberately requires a fresh coordinator-issued grant, and the digest never
// reaches a log, a diagnostic, or durable state.

import { createHash } from "node:crypto";
import { verifyLocalEndpointCapability } from "@roost/shared/local-endpoint";
import { log } from "@roost/shared/log";
import type { DLocalTerminalGrant } from "@roost/shared/proto/worker_transport_pb";
import { monoNowMs } from "./util/mono.ts";

/** One browser tab holds one grant and renews it in place, so these ceilings
 * only bound a coordinator that keeps minting grants nobody redeems. */
const MAX_GRANTS = 256;
const MAX_SESSIONS_PER_GRANT = 256;
const MAX_ID_LENGTH = 128;
const MAX_TTL_MS = 24 * 60 * 60_000;
const SHA256_HEX = /^[0-9a-f]{64}$/;

export interface LocalTerminalGrant {
	readonly grantId: string;
	/** The only sessions a socket holding this grant may reach. */
	readonly sessionIds: readonly string[];
	readonly deviceFingerprint: string;
	readonly tabId: string;
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

interface StoredGrant extends LocalTerminalGrant {
	/** Private to this module on purpose: the digest is the whole credential
	 * the worker holds, so every widening of its reach widens what a leak
	 * grants. It is never returned, logged, or serialized. */
	readonly secretSha256: string;
}

export class LocalTerminalGrantStore {
	private readonly grants = new Map<string, StoredGrant>();

	/** Monotonic by default: a TTL is a duration, so a host clock step must not
	 * extend or expire a grant (util/mono.ts). */
	constructor(private readonly now: () => number = monoNowMs) {}

	/** Install one coordinator-authorized grant, replacing any earlier grant
	 * with the same id (that is how renewal lands). Throws so the caller can
	 * answer the coordinator with WRpcError rather than a silent accept. */
	install(frame: DLocalTerminalGrant): LocalTerminalGrant {
		const sessionIds = validSessionIds(frame.sessionIds);
		if (!validId(frame.grantId)) throw new Error("grant_id is invalid");
		if (!SHA256_HEX.test(frame.secretSha256)) {
			throw new Error("secret_sha256 must be a lowercase hex SHA-256 digest");
		}
		if (!sessionIds) throw new Error("session_ids is invalid");
		if (!validId(frame.deviceFingerprint)) throw new Error("device_fingerprint is invalid");
		if (!validId(frame.tabId)) throw new Error("tab_id is invalid");
		if (!Number.isInteger(frame.ttlMs) || frame.ttlMs <= 0 || frame.ttlMs > MAX_TTL_MS) {
			throw new Error(`ttl_ms must be within 1..${MAX_TTL_MS}`);
		}
		this.sweep();
		if (!this.grants.has(frame.grantId) && this.grants.size >= MAX_GRANTS) {
			throw new Error("local terminal grant capacity is full");
		}
		const grant: StoredGrant = {
			grantId: frame.grantId,
			secretSha256: frame.secretSha256,
			sessionIds,
			deviceFingerprint: frame.deviceFingerprint,
			tabId: frame.tabId,
			expiresAtMs: this.now() + frame.ttlMs,
		};
		this.grants.set(grant.grantId, grant);
		log.info("local-terminal", "grant_installed", {
			grant_id: grant.grantId,
			device_fingerprint: grant.deviceFingerprint,
			tab_id: grant.tabId,
			sessions: sessionIds.length,
			ttl_ms: frame.ttlMs,
		});
		return grant;
	}

	/** Fixed vocabulary: the reason reaches the browser as a close frame, so it
	 * names WHICH check failed without echoing anything the caller sent. */
	verify(credential: LocalTerminalCredential): LocalTerminalGrantVerdict {
		const grant = this.grants.get(credential.grantId);
		if (!grant) return { ok: false, reason: "unknown local terminal grant" };
		if (grant.expiresAtMs <= this.now()) {
			this.grants.delete(grant.grantId);
			return { ok: false, reason: "local terminal grant expired" };
		}
		if (grant.deviceFingerprint !== credential.deviceFingerprint) {
			return { ok: false, reason: "grant is bound to another device" };
		}
		if (grant.tabId !== credential.tabId) {
			return { ok: false, reason: "grant is bound to another tab" };
		}
		// The worker holds a digest, never the secret, so the comparison is
		// digest-against-digest through the one timing-safe comparator.
		const received = createHash("sha256").update(credential.secret).digest("hex");
		if (!verifyLocalEndpointCapability(grant.secretSha256, received)) {
			return { ok: false, reason: "local terminal grant secret mismatch" };
		}
		return { ok: true, grant };
	}

	/** Drop every grant this device held. The caller closes their sockets. */
	revokeDevice(deviceFingerprint: string): readonly string[] {
		const revoked: string[] = [];
		for (const grant of [...this.grants.values()]) {
			if (grant.deviceFingerprint !== deviceFingerprint) continue;
			this.grants.delete(grant.grantId);
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

	count(): number {
		this.sweep();
		return this.grants.size;
	}

	clear(): void {
		this.grants.clear();
	}

	private sweep(): void {
		const now = this.now();
		for (const grant of [...this.grants.values()]) {
			if (grant.expiresAtMs <= now) this.grants.delete(grant.grantId);
		}
	}
}

function validId(value: string): boolean {
	return value.length > 0 && value.length <= MAX_ID_LENGTH;
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
