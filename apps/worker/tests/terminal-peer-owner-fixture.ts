// Deterministic node-datachannel fake for terminal-peer owner tests.
// It exposes native callback ordering and peer state without loading an N-API addon
// or opening UDP, while retaining fixture-only SDP values for log-safety assertions.

import type { TerminalPeerExpectedTuple } from "../src/terminal-peer-connection.ts";
import type { TerminalPeerPacketPort } from "../src/terminal-peer-packet-port.ts";
import type { TerminalPeerNative } from "../src/terminal-peer-native.ts";

export const OFFER_FINGERPRINT = Array.from({ length: 32 }, (_unused, index) => index.toString(16).padStart(2, "0"))
	.join(":");
export const ICE_PASSWORD = "p".repeat(22);
export const OFFER_SDP = [
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
export const ANSWER_SDP = OFFER_SDP.replace("a=setup:actpass", "a=setup:active");

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

export interface FakeDataChannelConfig {
	readonly negotiated?: boolean;
	readonly id?: number;
	readonly unordered?: boolean;
	readonly protocol?: string;
}

export class FakePeerConnection {
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

export interface FakeNativeFixture {
	native: TerminalPeerNative;
	readonly peers: FakePeerConnection[];
	readonly events: string[];
	afterPeerCreated: ((peer: FakePeerConnection) => void) | undefined;
	readonly expectedTuples: TerminalPeerExpectedTuple[];
	readonly ports: TerminalPeerPacketPort[];
	deferNewPeers: boolean;
	cleanupCalls: number;
}

export function createFakeNativeFixture(): FakeNativeFixture {
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
