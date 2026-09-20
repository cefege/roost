// Lifecycle owner for authenticated native browser terminal peers.
// Coordinator and grant checks are injected so this module owns no loopback socket
// registry; it reserves bounded peers, answers one offer, and retires only that peer.

import { create } from "@bufbuild/protobuf";
import { log } from "@roost/shared/log";
import {
	TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER,
	TERMINAL_PEER_MAX_NEGOTIATIONS_PER_WORKER,
	TERMINAL_PEER_NATIVE_ANSWER_DEADLINE_MS,
	parseTerminalPeerStunUrls,
} from "@roost/shared/terminal-peer";
import { inspectTerminalPeerSdp } from "@roost/shared/terminal-peer-sdp";
import {
	WLocalTerminalPeerAnswerSchema,
	type DLocalTerminalPeerCancel,
	type DLocalTerminalPeerOffer,
	type WLocalTerminalPeerAnswer,
} from "@roost/shared/proto/worker_transport_pb";
import type { TerminalRequestBudget } from "./transport/coord-link-types.ts";
import {
	TerminalPeerConnection,
	TerminalPeerConnectionError,
	type OpenTerminalPeerPort,
	type TerminalPeerConnectionConfig,
	type TerminalPeerExpectedTuple,
} from "./terminal-peer-connection.ts";
import { loadTerminalPeerNative, type TerminalPeerNative } from "./terminal-peer-native.ts";
import { TerminalPeerPacketBudget } from "./terminal-peer-packet-budget.ts";
import { validTerminalPeerOfferIdentity } from "./terminal-peer-request-validation.ts";
import type { TerminalPeerTestFaultState } from "./terminal-peer-test-faults.ts";
import type { TerminalPeerMalformedPacketKind } from "./terminal-peer-packet-test-fault.ts";

export type TerminalPeerBootstrapState = "disabled" | "native_unavailable" | "ready";
export type TerminalPeerOfferFailureReason =
	| "disabled"
	| "native_unavailable"
	| "invalid_offer"
	| "grant_unavailable"
	| "capacity"
	| "expired"
	| "connection_superseded"
	| "ice_failed";
export type TerminalPeerGrantAuthorization = "authorized" | "grant_unavailable" | "expired";

export class TerminalPeerOfferError extends Error {
	constructor(readonly reason: TerminalPeerOfferFailureReason) {
		super(`terminal peer offer failed: ${reason}`);
		this.name = "TerminalPeerOfferError";
	}
}

export interface TerminalPeerOwnerDeps {
	readonly processEpoch: string;
	readonly enabled: boolean;
	readonly bindAddress?: string;
	readonly portRange?: { readonly min: number; readonly max: number };
	/** Checks the exact authenticated coordinator connection that delivered a request. */
	readonly isCurrentCoordinator: (connectionGeneration: string) => boolean;
	/** Reads the live grant scope without exposing its digest or secret. */
	readonly authorizeGrant: (request: DLocalTerminalPeerOffer) => TerminalPeerGrantAuthorization;
	readonly openPeerPort: OpenTerminalPeerPort;
	readonly nativeLoader?: () => Promise<TerminalPeerNative>;
	readonly packetBudget?: TerminalPeerPacketBudget;
	/** Source-smoke-only state that mutates one offer inside the authenticated worker boundary. */
	readonly testFaults?: TerminalPeerTestFaultState;
	readonly expireGrantForTest?: (grantId: string) => void;
}

interface PendingPeer {
	readonly request: DLocalTerminalPeerOffer;
	readonly budget: TerminalRequestBudget;
	readonly expectedTuple: TerminalPeerExpectedTuple;
	readonly config: TerminalPeerConnectionConfig;
	cancelled: boolean;
	connection: TerminalPeerConnection | undefined;
}

interface ActivePeer {
	readonly expectedTuple: TerminalPeerExpectedTuple;
	readonly connection: TerminalPeerConnection;
}


/** One worker's bounded direct-peer owner. Bootstrap is explicit for capability publication and lazy on offer. */
export class TerminalPeerOwner {
	private readonly packetBudget: TerminalPeerPacketBudget;
	private readonly pending = new Map<string, PendingPeer>();
	private readonly active = new Map<string, ActivePeer>();
	private bootstrapPromise: Promise<TerminalPeerBootstrapState> | undefined;
	private bootstrapState: "idle" | TerminalPeerBootstrapState = "idle";
	private native: TerminalPeerNative | undefined;
	private nativeCleaned = false;
	private disposed = false;

	constructor(private readonly deps: TerminalPeerOwnerDeps) {
		this.packetBudget = deps.packetBudget ?? new TerminalPeerPacketBudget();
	}

	get capabilityState(): TerminalPeerBootstrapState | "idle" {
		return this.bootstrapState;
	}

	get establishedCount(): number {
		return this.active.size;
	}

	get negotiationCount(): number {
		return this.pending.size;
	}

	injectMalformedPacketForTest(kind: TerminalPeerMalformedPacketKind): boolean {
		for (const active of this.active.values()) {
			if (active.connection.injectMalformedPacketForTest(kind)) return true;
		}
		return false;
	}

	setHistoryDeliveryPausedForTest(paused: boolean): void {
		for (const active of this.active.values()) active.connection.setHistoryDeliveryPausedForTest(paused);
	}

	async bootstrap(): Promise<TerminalPeerBootstrapState> {
		if (!this.deps.enabled) {
			if (this.bootstrapState === "idle") {
				this.bootstrapState = "disabled";
				log.info("terminal-peer", "native_disabled", {});
			}
			return "disabled";
		}
		if (this.disposed) return "native_unavailable";
		this.bootstrapPromise ??= this.loadNative();
		return await this.bootstrapPromise;
	}

	async offer(
		request: DLocalTerminalPeerOffer,
		budget: TerminalRequestBudget,
	): Promise<WLocalTerminalPeerAnswer> {
		const offerFault = this.deps.testFaults?.consumeOfferFault() ?? null;
		if (offerFault !== null) log.info("terminal-peer", "offer_fault_applied", { fault: offerFault });
		if (offerFault === "invalid_sdp") request = { ...request, offerSdp: "smoke-invalid-sdp" };
		if (offerFault === "missing_grant") request = { ...request, grantId: "smoke-missing-grant" };
		if (offerFault === "expired_grant") this.deps.expireGrantForTest?.(request.grantId);
		const admissionFailure = this.admissionFailure(request, budget);
		if (admissionFailure !== null) throw new TerminalPeerOfferError(admissionFailure);
		let remoteFingerprint: string;
		let stunUrls: string[];
		try {
			if (request.stunUrls.length > 4) throw new Error("too many STUN URLs");
			for (const stunUrl of request.stunUrls) {
				if (stunUrl.length === 0 || stunUrl.includes(",")) throw new Error("invalid STUN URL");
			}
			remoteFingerprint = inspectTerminalPeerSdp(request.offerSdp).fingerprintSha256;
			stunUrls = parseTerminalPeerStunUrls(request.stunUrls.join(","));
		} catch {
			throw new TerminalPeerOfferError("invalid_offer");
		}
		if (
			this.pending.size >= TERMINAL_PEER_MAX_NEGOTIATIONS_PER_WORKER
			|| this.active.size + this.pending.size >= TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER
			|| this.pending.has(request.requestId)
			|| this.active.has(request.peerId)
			|| this.hasPendingPeerId(request.peerId)
			|| this.hasPeerForActor(request.deviceFingerprint, request.tabId)
		) {
			throw new TerminalPeerOfferError("capacity");
		}
		const expectedTuple: TerminalPeerExpectedTuple = {
			peerId: request.peerId,
			grantId: request.grantId,
			deviceFingerprint: request.deviceFingerprint,
			tabId: request.tabId,
			workerEpoch: offerFault === "identity_mismatch"
				? `${request.workerEpoch}-smoke-mismatch`
				: request.workerEpoch,
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
		log.info("terminal-peer", "peer_negotiating", { pending: this.pending.size });
		try {
			const bootstrapState = await this.bootstrap();
			this.assertCurrent(pending);
			if (bootstrapState !== "ready" || !this.native) {
				throw new TerminalPeerOfferError(bootstrapState === "disabled" ? "disabled" : "native_unavailable");
			}
			const peerBudget = this.packetBudget.createPeerBudget();
			let connection: TerminalPeerConnection | undefined;
			try {
				connection = new TerminalPeerConnection({
					native: this.native,
					peerId: request.peerId,
					expectedTuple,
					expectedRemoteFingerprint: remoteFingerprint,
					config: pending.config,
					packetBudget: peerBudget,
					openPeerPort: this.deps.openPeerPort,
					onClosed: () => {
						if (connection) this.handleConnectionClosed(pending, connection);
					},
					shouldBlackholeOutgoing: () => this.deps.testFaults?.peerPacketsBlackholed() === true,
				});
			} catch (error) {
				peerBudget.dispose();
				throw error;
			}
			if (!connection) throw new TerminalPeerOfferError("ice_failed");
			pending.connection = connection;
			const answerSdp = await connection.answer(request.offerSdp, this.nativeAnswerDeadline(budget));
			if (connection.isClosed) throw new TerminalPeerOfferError("ice_failed");
			this.assertCurrent(pending);
			if (this.pending.get(request.requestId) !== pending) {
				throw new TerminalPeerOfferError("connection_superseded");
			}
			this.pending.delete(request.requestId);
			this.active.set(request.peerId, { expectedTuple, connection });
			log.info("terminal-peer", "peer_established", { peers: this.active.size });
			return create(WLocalTerminalPeerAnswerSchema, {
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
			log.warn("terminal-peer", "peer_offer_refused", { reason, pending: this.pending.size });
			throw new TerminalPeerOfferError(reason);
		}
	}

	cancel(request: DLocalTerminalPeerCancel): void {
		const pending = this.pending.get(request.requestId);
		if (!pending || !this.matchesCancel(pending, request)) return;
		pending.cancelled = true;
		this.pending.delete(request.requestId);
		pending.connection?.close("connection_superseded");
		log.info("terminal-peer", "peer_cancelled", { pending: this.pending.size });
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
		if (closed > 0) log.info("terminal-peer", "peer_device_revoked", { peers: closed });
	}

	cancelPendingForCoordinator(_reason: string): void {
		let cancelled = 0;
		for (const pending of [...this.pending.values()]) {
			pending.cancelled = true;
			this.pending.delete(pending.request.requestId);
			pending.connection?.close("connection_superseded");
			cancelled += 1;
		}
		if (cancelled > 0) log.info("terminal-peer", "peer_coordinator_detached", { pending: cancelled });
	}

	dispose(): void {
		if (this.disposed) return;
		this.disposed = true;
		this.cancelPendingForCoordinator("worker_disposed");
		for (const active of [...this.active.values()]) active.connection.close("connection_superseded");
		this.active.clear();
		this.packetBudget.dispose();
		if (this.native) this.cleanupNative(this.native);
		log.info("terminal-peer", "peer_owner_disposed", {});
	}

	private async loadNative(): Promise<TerminalPeerBootstrapState> {
		try {
			const native = await (this.deps.nativeLoader ?? loadTerminalPeerNative)();
			if (this.disposed) {
				this.cleanupNative(native);
				return "native_unavailable";
			}
			this.native = native;
			this.bootstrapState = "ready";
			log.info("terminal-peer", "native_ready", {});
			return "ready";
		} catch {
			this.bootstrapState = "native_unavailable";
			log.warn("terminal-peer", "native_unavailable", {});
			return "native_unavailable";
		}
	}

	private admissionFailure(
		request: DLocalTerminalPeerOffer,
		budget: TerminalRequestBudget,
	): TerminalPeerOfferFailureReason | null {
		if (this.disposed || request.workerEpoch !== this.deps.processEpoch) return "connection_superseded";
		if (!this.deps.enabled) return "disabled";
		if (!validTerminalPeerOfferIdentity(request)) return "invalid_offer";
		try {
			if (!this.deps.isCurrentCoordinator(request.connectionGeneration) || !budget.isCurrentConnection()) {
				return "connection_superseded";
			}
			const remainingMs = budget.remainingMs();
			if (!Number.isFinite(remainingMs) || remainingMs <= 0) return "ice_failed";
			const grantAuthorization = this.deps.authorizeGrant(request);
			if (grantAuthorization === "authorized") return null;
			return grantAuthorization === "expired" ? "expired" : "grant_unavailable";
		} catch {
			return "connection_superseded";
		}
	}

	private assertCurrent(pending: PendingPeer): void {
		if (pending.cancelled || this.pending.get(pending.request.requestId) !== pending) {
			throw new TerminalPeerOfferError("connection_superseded");
		}
		const failure = this.admissionFailure(pending.request, pending.budget);
		if (failure !== null) throw new TerminalPeerOfferError(failure);
	}

	private nativeAnswerDeadline(budget: TerminalRequestBudget): number {
		try {
			return Math.min(TERMINAL_PEER_NATIVE_ANSWER_DEADLINE_MS, budget.remainingMs());
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

	private hasPeerForActor(deviceFingerprint: string, tabId: string): boolean {
		for (const pending of this.pending.values()) {
			if (
				pending.expectedTuple.deviceFingerprint === deviceFingerprint
				&& pending.expectedTuple.tabId === tabId
			) return true;
		}
		for (const active of this.active.values()) {
			if (
				active.expectedTuple.deviceFingerprint === deviceFingerprint
				&& active.expectedTuple.tabId === tabId
			) return true;
		}
		return false;
	}

	private matchesCancel(pending: PendingPeer, request: DLocalTerminalPeerCancel): boolean {
		return pending.request.connectionGeneration === request.connectionGeneration
			&& pending.request.workerEpoch === request.workerEpoch
			&& pending.request.peerId === request.peerId;
	}

	private handleConnectionClosed(pending: PendingPeer, connection: TerminalPeerConnection): void {
		const active = this.active.get(pending.request.peerId);
		if (active?.connection === connection) {
			this.active.delete(pending.request.peerId);
			log.info("terminal-peer", "peer_closed", { peers: this.active.size });
		}
	}

	private offerFailureReason(error: unknown): TerminalPeerOfferFailureReason {
		if (error instanceof TerminalPeerOfferError) return error.reason;
		if (error instanceof TerminalPeerConnectionError) return error.reason;
		return "ice_failed";
	}


	private cleanupNative(native: TerminalPeerNative): void {
		if (this.nativeCleaned) return;
		this.nativeCleaned = true;
		try { native.cleanup(); } catch { /* worker shutdown must continue after native teardown */ }
	}
}
