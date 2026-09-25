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
} from "@roost/protocol/proto/worker_transport_pb";
import { TERMINAL_PEER_DATA_CHANNELS } from "@roost/protocol/terminal-peer";
import type { TerminalPeerNative } from "../../../src/terminal/peer/terminal-peer-native.ts";
import { TerminalPeerOfferError, TerminalPeerOwner } from "../../../src/terminal/peer/terminal-peer-owner.ts";
import {
	ANSWER_SDP,
	createFakeNativeFixture,
	ICE_PASSWORD,
	OFFER_SDP,
	type FakeNativeFixture,
} from "./terminal-peer-owner-fixture.ts";

const WORKER_EPOCH = "11111111-1111-4111-8111-111111111111";
const DEVICE_FINGERPRINT = "a".repeat(64);

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
		const nativeCloseRequest = peerOffer();
		await owner.offer(nativeCloseRequest, liveBudget());
		expect(owner.establishedCount).toBe(2);
		fake.ports[0]!.close();
		expect(fake.peers[0]!.closed).toBe(true);
		expect(fake.peers[1]!.closed).toBe(false);
		expect(owner.establishedCount).toBe(1);

		const capturedLogs: string[] = [];
		const originalLog = console.log;
		console.log = (...values: unknown[]) => { capturedLogs.push(values.map(String).join(" ")); };
		try {
			fake.peers[1]!.emitUnsolicitedDataChannel();
			fake.peers[1]!.emitUnsolicitedDataChannel();
		} finally {
			console.log = originalLog;
		}

		expect(owner.establishedCount).toBe(0);
		expect(capturedLogs).toHaveLength(1);
		const peerClosed = JSON.parse(capturedLogs[0]!) as Record<string, unknown>;
		expect(peerClosed).toMatchObject({
			level: "info",
			target: "terminal-peer",
			msg: "peer_closed",
			peers: 0,
			reason: "ice_failed",
		});
		const closedRecord = capturedLogs[0]!;
		expect(closedRecord).not.toContain(OFFER_SDP);
		expect(closedRecord).not.toContain("192.0.2.8");
		expect(closedRecord).not.toContain(ICE_PASSWORD);
		expect(closedRecord).not.toContain(nativeCloseRequest.grantId);
		owner.dispose();
	});
});
