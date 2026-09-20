// Native node-datachannel answerer for one authenticated browser peer.
// It creates the fixed data channels before applying remote SDP, binds the
// browser fingerprint at DTLS connection, and delegates framing to its packet port.

import { randomUUID } from "node:crypto";
import {
	TERMINAL_PEER_DATA_CHANNELS,
	TERMINAL_PEER_ICE_GATHERING_DEADLINE_MS,
	TERMINAL_PEER_MAX_MESSAGE_SIZE,
} from "@roost/shared/terminal-peer";
import {
	inspectTerminalPeerSdp,
	normalizeTerminalPeerSha256Fingerprint,
} from "@roost/shared/terminal-peer-sdp";
import type { TerminalPeerNative } from "./terminal-peer-native.ts";
import type { TerminalPeerPacketPeerBudget } from "./terminal-peer-packet-budget.ts";
import type { TerminalPeerMalformedPacketKind } from "./terminal-peer-packet-test-fault.ts";
import {
	TerminalPeerPacketPort,
	type TerminalPeerNativeDataChannel,
	type TerminalPeerNativeDataChannels,
	type TerminalPeerPacketIngress,
} from "./terminal-peer-packet-port.ts";

export type TerminalPeerConnectionFailureReason = "ice_failed" | "connection_superseded";

export class TerminalPeerConnectionError extends Error {
	constructor(readonly reason: TerminalPeerConnectionFailureReason) {
		super(`terminal peer connection failed: ${reason}`);
		this.name = "TerminalPeerConnectionError";
	}
}

export interface TerminalPeerExpectedTuple {
	readonly peerId: string;
	readonly grantId: string;
	readonly deviceFingerprint: string;
	readonly tabId: string;
	readonly workerEpoch: string;
}

export interface TerminalPeerConnectionConfig {
	readonly stunUrls: readonly string[];
	readonly bindAddress?: string;
	readonly portRange?: { readonly min: number; readonly max: number };
}

export type OpenTerminalPeerPort = (
	port: TerminalPeerPacketPort,
	expectedTuple: TerminalPeerExpectedTuple,
) => TerminalPeerPacketIngress;

export interface TerminalPeerConnectionDeps {
	readonly native: TerminalPeerNative;
	readonly peerId: string;
	readonly expectedTuple: TerminalPeerExpectedTuple;
	readonly expectedRemoteFingerprint: string;
	readonly config: TerminalPeerConnectionConfig;
	readonly packetBudget: TerminalPeerPacketPeerBudget;
	readonly openPeerPort: OpenTerminalPeerPort;
	readonly onClosed: (reason: TerminalPeerConnectionFailureReason) => void;
	readonly socketId?: string;
	/** Source-smoke-only packet loss boundary for an established peer. */
	readonly shouldBlackholeOutgoing?: () => boolean;
}

interface AnswerWaiter {
	resolve(answerSdp: string): void;
	reject(error: TerminalPeerConnectionError): void;
}

/** One immutable offer/answer exchange. Failures retire this peer rather than renegotiating it. */
export class TerminalPeerConnection {
	readonly port!: TerminalPeerPacketPort;
	private readonly peer: InstanceType<TerminalPeerNative["PeerConnection"]>;
	private answerWaiter: AnswerWaiter | undefined;
	private gatheringTimer: NodeJS.Timeout | undefined;
	private fingerprintVerified = false;
	private closed = false;
	private closedNotified = false;

	constructor(private readonly deps: TerminalPeerConnectionDeps) {
		this.peer = new deps.native.PeerConnection(`roost-terminal-peer-${deps.peerId}`, {
			iceServers: [...deps.config.stunUrls],
			disableAutoNegotiation: true,
			enableIceTcp: false,
			disableFingerprintVerification: false,
			iceTransportPolicy: "all",
			maxMessageSize: TERMINAL_PEER_MAX_MESSAGE_SIZE,
			...(deps.config.bindAddress === undefined ? {} : { bindAddress: deps.config.bindAddress }),
			...(deps.config.portRange === undefined
				? {}
				: {
					portRangeBegin: deps.config.portRange.min,
					portRangeEnd: deps.config.portRange.max,
				}),
		});
		this.installPeerCallbacks();
		let packetPort: TerminalPeerPacketPort | undefined;
		try {
			const channels = this.createDataChannels();
			packetPort = new TerminalPeerPacketPort({
				socketId: deps.socketId ?? randomUUID(),
				channels,
				packetBudget: deps.packetBudget,
				onChannelOpen: () => { this.verifyRemoteFingerprint(); },
				onClosed: () => { this.close("connection_superseded"); },
				onFatal: () => { this.fail("ice_failed"); },
				shouldBlackholeOutgoing: deps.shouldBlackholeOutgoing,
			});
			this.port = packetPort;
			const ingress = deps.openPeerPort(packetPort, deps.expectedTuple);
			if (!ingress || typeof ingress.onMessage !== "function") {
				throw new Error("terminal peer port ingress is unavailable");
			}
			packetPort.attachIngress(ingress);
		} catch (error) {
			const reason = error instanceof TerminalPeerConnectionError ? error.reason : "connection_superseded";
			packetPort?.close(undefined, reason);
			deps.packetBudget.dispose();
			try { this.peer.close(); } catch { /* native constructor failure has no peer to recover */ }
			throw new TerminalPeerConnectionError(reason);
		}
	}


	injectMalformedPacketForTest(kind: TerminalPeerMalformedPacketKind): boolean {
		return this.port.injectMalformedPacketForTest(kind);
	}

	setHistoryDeliveryPausedForTest(paused: boolean): void {
		this.port.setHistoryDeliveryPausedForTest(paused);
	}
	get isClosed(): boolean {
		return this.closed;
	}

	answer(offerSdp: string, deadlineMs: number): Promise<string> {
		if (this.closed || this.answerWaiter !== undefined) {
			return Promise.reject(new TerminalPeerConnectionError("connection_superseded"));
		}
		const answer = new Promise<string>((resolve, reject) => {
			this.answerWaiter = { resolve, reject };
		});
		try {
			this.peer.setRemoteDescription(offerSdp, "offer");
			this.peer.setLocalDescription("answer");
		} catch {
			this.fail("ice_failed");
			return answer;
		}
		if (this.closed || this.answerWaiter === undefined) return answer;
		const gatheringDeadlineMs = Math.min(TERMINAL_PEER_ICE_GATHERING_DEADLINE_MS, deadlineMs);
		if (!Number.isFinite(gatheringDeadlineMs) || gatheringDeadlineMs <= 0) {
			this.fail("ice_failed");
			return answer;
		}
		this.gatheringTimer = setTimeout(() => {
			this.gatheringTimer = undefined;
			const partialAnswer = this.answerWithCandidates();
			if (partialAnswer === null) this.fail("ice_failed");
			else this.finishAnswer(partialAnswer);
		}, gatheringDeadlineMs);
		this.gatheringTimer.unref?.();
		this.resolveCompleteAnswer();
		return answer;
	}

	close(reason: TerminalPeerConnectionFailureReason = "connection_superseded"): void {
		if (this.closed) return;
		this.closed = true;
		clearTimeout(this.gatheringTimer);
		this.gatheringTimer = undefined;
		const waiter = this.answerWaiter;
		this.answerWaiter = undefined;
		waiter?.reject(new TerminalPeerConnectionError(reason));
		this.port.close(undefined, reason);
		try { this.peer.close(); } catch { /* native peer is already terminal */ }
		this.notifyClosed(reason);
	}

	private installPeerCallbacks(): void {
		this.peer.onLocalDescription(() => { this.resolveCompleteAnswer(); });
		this.peer.onGatheringStateChange(() => { this.resolveCompleteAnswer(); });
		this.peer.onStateChange((state) => {
			if (state === "connected") this.verifyRemoteFingerprint();
			else if (state === "failed" || state === "closed") this.fail("ice_failed");
		});
		this.peer.onIceStateChange((state) => {
			if (state === "failed") this.fail("ice_failed");
		});
		this.peer.onDataChannel(() => { this.fail("ice_failed"); });
		this.peer.onTrack(() => { this.fail("ice_failed"); });
	}

	private createDataChannels(): TerminalPeerNativeDataChannels {
		const channels: Partial<TerminalPeerNativeDataChannels> = {};
		for (const definition of TERMINAL_PEER_DATA_CHANNELS) {
			channels[definition.lane] = this.peer.createDataChannel(definition.label, {
				negotiated: true,
				id: definition.id,
				unordered: false,
				protocol: definition.protocol,
			}) as unknown as TerminalPeerNativeDataChannel;
		}
		if (!channels.control || !channels.terminal || !channels.history) {
			throw new TerminalPeerConnectionError("ice_failed");
		}
		return channels as TerminalPeerNativeDataChannels;
	}

	private resolveCompleteAnswer(): void {
		if (this.closed || this.answerWaiter === undefined || this.peer.gatheringState() !== "complete") return;
		const answerSdp = this.answerWithCandidates();
		if (answerSdp !== null) this.finishAnswer(answerSdp);
	}

	private answerWithCandidates(): string | null {
		const description = this.peer.localDescription();
		if (!description || description.type !== "answer") return null;
		try {
			return inspectTerminalPeerSdp(description.sdp).candidateCount > 0 ? description.sdp : null;
		} catch {
			return null;
		}
	}

	private finishAnswer(answerSdp: string): void {
		const waiter = this.answerWaiter;
		if (!waiter) return;
		this.answerWaiter = undefined;
		clearTimeout(this.gatheringTimer);
		this.gatheringTimer = undefined;
		waiter.resolve(answerSdp);
	}

	private verifyRemoteFingerprint(): void {
		if (this.closed || this.fingerprintVerified) return;
		try {
			const fingerprint = this.peer.remoteFingerprint();
			if (
				fingerprint.algorithm.toLowerCase() !== "sha-256"
				|| normalizeTerminalPeerSha256Fingerprint(fingerprint.value) !== this.deps.expectedRemoteFingerprint
			) {
				this.fail("ice_failed");
				return;
			}
			this.fingerprintVerified = true;
		} catch {
			this.fail("ice_failed");
		}
	}

	private fail(reason: TerminalPeerConnectionFailureReason): void {
		this.close(reason);
	}

	private notifyClosed(reason: TerminalPeerConnectionFailureReason): void {
		if (this.closedNotified) return;
		this.closedNotified = true;
		this.deps.onClosed(reason);
	}
}
