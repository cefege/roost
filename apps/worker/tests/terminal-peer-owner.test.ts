// Focused owner tests with a deterministic node-datachannel fake.
// They prove offer fencing and peer lifecycle without loading an N-API addon or
// opening UDP, while checking the callback ordering production uses for direct ports.

import { create } from "@bufbuild/protobuf";
import { describe, expect, test } from "bun:test";
import { randomUUID } from "node:crypto";
import {
	DLocalTerminalPeerCancelSchema,
	DLocalTerminalPeerOfferSchema,
	type DLocalTerminalPeerOffer,
} from "@roost/shared/proto/worker_transport_pb";
import { TERMINAL_PEER_DATA_CHANNELS } from "@roost/shared/terminal-peer";
import type { TerminalPeerExpectedTuple } from "../src/terminal-peer-connection.ts";
import type { TerminalPeerPacketPort } from "../src/terminal-peer-packet-port.ts";
import type { TerminalPeerNative } from "../src/terminal-peer-native.ts";
import { TerminalPeerOfferError, TerminalPeerOwner } from "../src/terminal-peer-owner.ts";

const WORKER_EPOCH = "11111111-1111-4111-8111-111111111111";
const DEVICE_FINGERPRINT = "a".repeat(64);
const OFFER_FINGERPRINT = Array.from({ length: 32 }, (_unused, index) => index.toString(16).padStart(2, "0"))
	.join(":");
const ICE_PASSWORD = "p".repeat(22);
const OFFER_SDP = [
	"v=0",
	"o=- 1 2 IN IP4 127.0.0.1",
	"s=-",
	"t=0 0",
	"m=application 9 UDP/DTLS/SCTP webrtc-datachannel",
	"a=setup:actpass",
	`a=fingerprint:sha-256 ${OFFER_FINGERPRINT}`,
	"a=ice-ufrag:offer-ufrag",
	`a=ice-pwd:${ICE_PASSWORD}`,
	"a=max-message-size:16384",
	"a=candidate:host 1 udp 2122260223 192.0.2.8 5000 typ host",
	"",
].join("\r\n");
const ANSWER_SDP = OFFER_SDP.replace("a=setup:actpass", "a=setup:active");

class FakeDataChannel {
	private openCallback: (() => void) | undefined;
	private closeCallback: (() => void) | undefined;
	private opened = false;
	private closed = false;

	close(): void {
		if (this.closed) return;
		this.closed = true;
		this.closeCallback?.();
	}

	sendMessageBinary(_bytes: Uint8Array): boolean {
		return true;
	}

	isOpen(): boolean {
		return this.opened && !this.closed;
	}

	bufferedAmount(): number {
		return 0;
	}

	setBufferedAmountLowThreshold(_bytes: number): void {}

	onOpen(callback: () => void): void {
		this.openCallback = callback;
	}

	onClosed(callback: () => void): void {
		this.closeCallback = callback;
	}

	onError(_callback: (error: string) => void): void {}

	onBufferedAmountLow(_callback: () => void): void {}

	onMessage(_callback: (message: string | Uint8Array | ArrayBuffer) => void): void {}

	emitOpen(): void {
		this.opened = true;
		this.openCallback?.();
	}

}

interface FakeDataChannelConfig {
	readonly negotiated?: boolean;
	readonly id?: number;
	readonly unordered?: boolean;
	readonly protocol?: string;
}

class FakePeerConnection {
	private localDescriptionCallback: ((sdp: string, type: string) => void) | undefined;
	private gatheringCallback: ((state: string) => void) | undefined;
	private stateCallback: ((state: string) => void) | undefined;
	private iceCallback: ((state: string) => void) | undefined;
	private dataChannelCallback: ((channel: FakeDataChannel) => void) | undefined;
	private local: { type: string; sdp: string } | null = null;
	private gathering = "new";
	afterAnswerSettled: (() => void) | undefined;
	deferGathering = false;
	readonly channels: Array<{ label: string; config: FakeDataChannelConfig; channel: FakeDataChannel }> = [];
	remoteFingerprintCalls = 0;
	closed = false;

	constructor(
		readonly name: string,
		readonly config: Record<string, unknown>,
		private readonly events: string[],
	) {}

	close(): void {
		if (this.closed) return;
		this.closed = true;
		this.stateCallback?.("closed");
	}

	setRemoteDescription(_sdp: string, _type: string): void {
		this.events.push("remote-description");
	}

	setLocalDescription(_type?: string): void {
		this.local = { type: "answer", sdp: ANSWER_SDP };
		this.localDescriptionCallback?.(ANSWER_SDP, "answer");
		if (this.deferGathering) return;
		this.completeGathering();
	}

	completeGathering(): void {
		this.gathering = "complete";
		this.gatheringCallback?.("complete");
		this.afterAnswerSettled?.();
	}

	localDescription(): { type: string; sdp: string } | null {
		return this.local;
	}

	gatheringState(): string {
		return this.gathering;
	}

	remoteFingerprint(): { algorithm: string; value: string } {
		this.remoteFingerprintCalls += 1;
		return { algorithm: "sha-256", value: OFFER_FINGERPRINT };
	}

	createDataChannel(label: string, config: FakeDataChannelConfig): FakeDataChannel {
		const channel = new FakeDataChannel();
		this.channels.push({ label, config, channel });
		return channel;
	}

	onLocalDescription(callback: (sdp: string, type: string) => void): void {
		this.localDescriptionCallback = callback;
	}

	onGatheringStateChange(callback: (state: string) => void): void {
		this.gatheringCallback = callback;
	}

	onStateChange(callback: (state: string) => void): void {
		this.stateCallback = callback;
	}

	onIceStateChange(callback: (state: string) => void): void {
		this.iceCallback = callback;
	}

	onDataChannel(callback: (channel: FakeDataChannel) => void): void {
		this.dataChannelCallback = callback;
	}

	onTrack(_callback: (track: unknown) => void): void {}

	emitConnected(): void {
		this.stateCallback?.("connected");
	}

	emitIceFailure(): void {
		this.iceCallback?.("failed");
	}

	emitUnsolicitedDataChannel(): void {
		this.dataChannelCallback?.(new FakeDataChannel());
	}

}

interface FakeNativeFixture {
	native: TerminalPeerNative;
	readonly peers: FakePeerConnection[];
	readonly events: string[];
	afterPeerCreated: ((peer: FakePeerConnection) => void) | undefined;
	readonly expectedTuples: TerminalPeerExpectedTuple[];
	readonly ports: TerminalPeerPacketPort[];
	deferNewPeers: boolean;
	cleanupCalls: number;
}

function createFakeNativeFixture(): FakeNativeFixture {
	const peers: FakePeerConnection[] = [];
	const events: string[] = [];
	const fixture: FakeNativeFixture = {
		peers,
		afterPeerCreated: undefined,
		deferNewPeers: false,
		expectedTuples: [],
		ports: [],
		events,
		cleanupCalls: 0,
		native: undefined as unknown as TerminalPeerNative,
	};
	class NativePeerConnection extends FakePeerConnection {
		constructor(name: string, config: Record<string, unknown>) {
			super(name, config, events);
			peers.push(this);
			fixture.afterPeerCreated?.(this);
			this.deferGathering = fixture.deferNewPeers;
		}
	}
	fixture.native = {
		PeerConnection: NativePeerConnection,
		preload: () => undefined,
		cleanup: () => { fixture.cleanupCalls += 1; },
		initLogger: () => undefined,
		getLibraryVersion: () => "fake",
		setSctpSettings: () => undefined,
	} as unknown as TerminalPeerNative;
	return fixture;
}

function peerOffer(overrides: Partial<DLocalTerminalPeerOffer> = {}): DLocalTerminalPeerOffer {
	return create(DLocalTerminalPeerOfferSchema, {
		requestId: randomUUID(),
		connectionGeneration: randomUUID(),
		workerEpoch: WORKER_EPOCH,
		grantId: randomUUID(),
		peerId: randomUUID(),
		deviceFingerprint: DEVICE_FINGERPRINT,
		tabId: randomUUID(),
		offerSdp: OFFER_SDP,
		budgetMs: 8_000,
		stunUrls: [],
		...overrides,
	});
}

function liveBudget(): { isCurrentConnection(): boolean; remainingMs(): number } {
	return {
		isCurrentConnection: () => true,
		remainingMs: () => 8_000,
	};
}

function ownerWithNative(fake: FakeNativeFixture, nativeLoader = async (): Promise<TerminalPeerNative> => fake.native, enabled = true) {
	return new TerminalPeerOwner({
		processEpoch: WORKER_EPOCH,
		enabled,
		bindAddress: "127.0.0.1",
		portRange: { min: 41000, max: 41001 },
		isCurrentCoordinator: () => true,
		authorizeGrant: () => "authorized",
		openPeerPort: (port, expectedTuple) => {
			fake.expectedTuples.push(expectedTuple);
			fake.ports.push(port);
			fake.events.push(`port:${expectedTuple.peerId}`);
			return { onMessage: () => undefined };
		},
		nativeLoader,
	});
}
describe("terminal peer owner", () => {
	test("binds the expected tuple before native SDP, creates fixed channels, and checks DTLS callbacks", async () => {
		const fake = createFakeNativeFixture();
		const owner = ownerWithNative(fake);
		const request = peerOffer();

		const answer = await owner.offer(request, liveBudget());
		const peer = fake.peers[0]!;
		expect(answer).toMatchObject({ peerId: request.peerId, workerEpoch: WORKER_EPOCH, answerSdp: ANSWER_SDP });
		expect(fake.events.indexOf(`port:${request.peerId}`)).toBeLessThan(fake.events.indexOf("remote-description"));
		expect(peer.config).toMatchObject({
			disableAutoNegotiation: true,
			enableIceTcp: false,
			disableFingerprintVerification: false,
			iceTransportPolicy: "all",
			maxMessageSize: 16_384,
			bindAddress: "127.0.0.1",
			portRangeBegin: 41000,
			portRangeEnd: 41001,
		});
		expect(peer.channels.map(({ label, config }) => ({ label, config }))).toEqual(
			TERMINAL_PEER_DATA_CHANNELS.map(({ label, id, protocol }) => ({
				label,
				config: { negotiated: true, id, unordered: false, protocol },
			})),
		);
		peer.emitConnected();
		for (const entry of peer.channels) entry.channel.emitOpen();
		expect(peer.remoteFingerprintCalls).toBeGreaterThan(0);
		expect(fake.expectedTuples).toEqual([{
			peerId: request.peerId,
			grantId: request.grantId,
			deviceFingerprint: request.deviceFingerprint,
			tabId: request.tabId,
			workerEpoch: request.workerEpoch,
		}]);
		await expect(owner.offer(peerOffer({ tabId: request.tabId }), liveBudget()))
			.rejects.toMatchObject({ reason: "capacity" });

		owner.revokeDevice(DEVICE_FINGERPRINT);
		expect(peer.closed).toBe(true);
		expect(owner.establishedCount).toBe(0);
		owner.dispose();
		owner.dispose();
		expect(fake.cleanupCalls).toBe(1);
	});

	test("reports bootstrap states and bounds pending offers", async () => {
		const fake = createFakeNativeFixture();
		let disabledLoaderCalled = false;
		const disabled = ownerWithNative(fake, async () => {
			disabledLoaderCalled = true;
			return fake.native;
		}, false);
		expect(await disabled.bootstrap()).toBe("disabled");
		expect(disabledLoaderCalled).toBe(false);
		const unavailable = ownerWithNative(fake, async () => { throw new Error("native unavailable"); });
		expect(await unavailable.bootstrap()).toBe("native_unavailable");
		const nativeGate = Promise.withResolvers<TerminalPeerNative>();
		const owner = ownerWithNative(fake, async () => await nativeGate.promise);
		await expect(owner.offer(peerOffer({ workerEpoch: randomUUID() }), liveBudget()))
			.rejects.toMatchObject({ reason: "connection_superseded" } satisfies Partial<TerminalPeerOfferError>);

		const pending = Array.from({ length: 4 }, () => peerOffer());
		const promises = pending.map((request) => owner.offer(request, liveBudget()));
		expect(owner.negotiationCount).toBe(4);
		await expect(owner.offer(peerOffer(), liveBudget()))
			.rejects.toMatchObject({ reason: "capacity" } satisfies Partial<TerminalPeerOfferError>);
		for (const request of pending) {
			owner.cancel(create(DLocalTerminalPeerCancelSchema, {
				requestId: request.requestId,
				connectionGeneration: request.connectionGeneration,
				workerEpoch: request.workerEpoch,
				peerId: request.peerId,
			}));
		}
		nativeGate.resolve(fake.native);
		for (const result of await Promise.allSettled(promises)) {
			expect(result.status).toBe("rejected");
			if (result.status === "rejected") {
				expect(result.reason).toMatchObject({ reason: "connection_superseded" });
			}
		}
		owner.dispose();
		expect(fake.cleanupCalls).toBe(1);
	});



	test("reserves the final established slot while an answer is still pending", async () => {
		const fake = createFakeNativeFixture();
		const owner = ownerWithNative(fake);
		for (let index = 0; index < 31; index += 1) await owner.offer(peerOffer(), liveBudget());
		fake.deferNewPeers = true;
		const heldAnswer = owner.offer(peerOffer(), liveBudget());
		expect(owner.negotiationCount).toBe(1);
		await expect(owner.offer(peerOffer(), liveBudget())).rejects.toMatchObject({ reason: "capacity" });
		fake.peers.at(-1)!.completeGathering();
		await heldAnswer;
		expect(owner.establishedCount).toBe(32);
		owner.dispose();
	});
	test("does not promote an answer whose peer closes before the offer continuation", async () => {
		const fake = createFakeNativeFixture();
		fake.afterPeerCreated = (peer) => { peer.afterAnswerSettled = () => { peer.emitIceFailure(); }; };
		const owner = ownerWithNative(fake);
		await expect(owner.offer(peerOffer(), liveBudget())).rejects.toMatchObject({ reason: "ice_failed" });
		expect(owner.establishedCount).toBe(0);
		owner.dispose();
	});
	test("retires one peer for an injected port close or native callback", async () => {
		const fake = createFakeNativeFixture();
		const owner = ownerWithNative(fake);
		await owner.offer(peerOffer(), liveBudget());
		await owner.offer(peerOffer(), liveBudget());
		expect(owner.establishedCount).toBe(2);
		fake.ports[0]!.close();
		expect(fake.peers[0]!.closed).toBe(true);
		expect(fake.peers[1]!.closed).toBe(false);
		expect(owner.establishedCount).toBe(1);
		fake.peers[1]!.emitUnsolicitedDataChannel();
		expect(owner.establishedCount).toBe(0);
		owner.dispose();
	});
});
