// Focused packet-port tests with native-channel fakes.
// They pin ownership at the native false-send boundary, frame rejection and the
// separate control reservation that prevents history from blocking terminal control.

import { describe, expect, test } from "bun:test";
import {
	TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES,
	TERMINAL_PEER_CHANNEL_WATERMARKS,
	TERMINAL_PEER_CONTROL_QUEUE_MAX_BYTES,
	TERMINAL_PEER_MAX_FLUSH_BYTES_PER_TURN,
	TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES,
	type TerminalPeerPacketLane,
} from "@roost/protocol/terminal-peer";
import { encodeTerminalPeerPacket, parseTerminalPeerPacket } from "@roost/protocol/terminal-peer-packets";
import { TerminalPeerPacketBudget } from "../../../src/terminal/peer/terminal-peer-packet-budget.ts";
import {
	TerminalPeerPacketPort,
	type TerminalPeerNativeDataChannel,
	type TerminalPeerNativeDataChannels,
} from "../../../src/terminal/peer/terminal-peer-packet-port.ts";

class FakeNativeChannel implements TerminalPeerNativeDataChannel {
	private openCallback: (() => void) | undefined;
	private closedCallback: (() => void) | undefined;
	private errorCallback: ((error: string) => void) | undefined;
	private lowCallback: (() => void) | undefined;
	private messageCallback: ((message: string | Uint8Array | ArrayBuffer) => void) | undefined;
	private opened = false;
	private closed = false;
	buffered = 0;
	threshold = 0;
	returnValues: boolean[] = [];
	readonly sent: Uint8Array[] = [];

	constructor(
		readonly lane: TerminalPeerPacketLane,
		private readonly sentOrder: TerminalPeerPacketLane[],
	) {}

	close(): void {
		if (this.closed) return;
		this.closed = true;
		this.opened = false;
		this.closedCallback?.();
	}

	sendMessageBinary(bytes: Buffer | Uint8Array): boolean {
		this.sent.push(new Uint8Array(bytes.buffer, bytes.byteOffset, bytes.byteLength));
		this.sentOrder.push(this.lane);
		return this.returnValues.shift() ?? true;
	}

	isOpen(): boolean {
		return this.opened && !this.closed;
	}

	bufferedAmount(): number {
		return this.buffered;
	}

	setBufferedAmountLowThreshold(bytes: number): void {
		this.threshold = bytes;
	}

	onOpen(callback: () => void): void {
		this.openCallback = callback;
	}

	onClosed(callback: () => void): void {
		this.closedCallback = callback;
	}

	onError(callback: (error: string) => void): void {
		this.errorCallback = callback;
	}

	onBufferedAmountLow(callback: () => void): void {
		this.lowCallback = callback;
	}

	onMessage(callback: (message: string | Uint8Array | ArrayBuffer) => void): void {
		this.messageCallback = callback;
	}

	emitOpen(): void {
		this.opened = true;
		this.openCallback?.();
	}

	emitLow(): void {
		this.buffered = 0;
		this.lowCallback?.();
	}

	emitMessage(message: string | Uint8Array | ArrayBuffer): void {
		this.messageCallback?.(message);
	}

}

interface PacketPortFixture {
	readonly budget: TerminalPeerPacketBudget;
	readonly channels: Record<TerminalPeerPacketLane, FakeNativeChannel>;
	readonly port: TerminalPeerPacketPort;
	readonly messages: Uint8Array[];
	readonly closeReasons: string[];
	readonly sentOrder: TerminalPeerPacketLane[];
}

function createPacketPortFixture(
	budget: TerminalPeerPacketBudget = new TerminalPeerPacketBudget(),
): PacketPortFixture {
	const sentOrder: TerminalPeerPacketLane[] = [];
	const channels = {
		control: new FakeNativeChannel("control", sentOrder),
		terminal: new FakeNativeChannel("terminal", sentOrder),
		history: new FakeNativeChannel("history", sentOrder),
	};
	const messages: Uint8Array[] = [];
	const closeReasons: string[] = [];
	const port = new TerminalPeerPacketPort({
		socketId: "peer-socket",
		channels: channels as TerminalPeerNativeDataChannels,
		packetBudget: budget.createPeerBudget(),
		onClosed: (reason) => { closeReasons.push(reason); },
	});
	port.attachIngress({ onMessage: (bytes) => { messages.push(bytes); } });
	for (const channel of Object.values(channels)) channel.emitOpen();
	return { budget, channels, port, messages, closeReasons, sentOrder };
}

function framedControl(messageId: number, payload: Uint8Array): Uint8Array {
	return encodeTerminalPeerPacket("control", {
		messageId,
		totalBytes: payload.byteLength,
		offsetBytes: 0,
	}, payload);
}

describe("terminal peer packet port", () => {
	test("commits a native false return once without retrying its accepted fragment", () => {
		const fixture = createPacketPortFixture();
		fixture.channels.control.returnValues.push(false);

		expect(fixture.port.send(Uint8Array.of(7, 8, 9), "control")).toBe("backpressured");
		expect(fixture.channels.control.sent).toHaveLength(1);
		expect(parseTerminalPeerPacket("control", fixture.channels.control.sent[0]!)).toMatchObject({
			messageId: 1,
			totalBytes: 3,
			payload: Uint8Array.of(7, 8, 9),
		});

		fixture.channels.control.emitLow();
		expect(fixture.channels.control.sent).toHaveLength(1);
		expect(fixture.port.send(Uint8Array.of(1), "control")).toBe("accepted");
		expect(fixture.channels.control.sent).toHaveLength(2);
	});

	test("reassembles framed control, rejects malformed packets, and releases a partial quota on close", () => {
		const fixture = createPacketPortFixture();
		fixture.channels.control.emitMessage(framedControl(1, Uint8Array.of(4, 5)));
		expect(fixture.messages).toEqual([Uint8Array.of(4, 5)]);
		const unauthenticated = createPacketPortFixture();
		unauthenticated.channels.control.emitMessage(framedControl(1, new Uint8Array(4_097)));
		expect(unauthenticated.port.open).toBe(false);
		expect(unauthenticated.closeReasons).toEqual(["unauthenticated_frame_too_large"]);


		fixture.port.markAuthenticated();
		const payload = new Uint8Array(TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES + 1).fill(3);
		fixture.channels.control.emitMessage(encodeTerminalPeerPacket("control", {
			messageId: 2,
			totalBytes: payload.byteLength,
			offsetBytes: 0,
		}, payload.slice(0, TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES)));
		expect(fixture.budget.snapshot("incoming").controlBytes).toBe(payload.byteLength);
		fixture.port.close();
		expect(fixture.budget.snapshot("incoming").retainedBytes).toBe(0);

		const healthy = createPacketPortFixture();
		const malformed = framedControl(1, Uint8Array.of(1));
		new DataView(malformed.buffer, malformed.byteOffset, 4).setUint32(0, 0, true);
		healthy.channels.control.emitMessage(malformed);
		expect(healthy.port.open).toBe(false);
		expect(healthy.closeReasons).toEqual(["packet_rejected"]);
	});

	test("keeps control reservation separate from application bytes and drains lanes by priority", () => {
		const budget = new TerminalPeerPacketBudget();
		const peerBudget = budget.createPeerBudget();
		const historyQuota = peerBudget.quota("outgoing", "history");
		const controlQuota = peerBudget.quota("outgoing", "control");
		expect(historyQuota.reserve(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES)).toBe(true);
		expect(controlQuota.reserve(TERMINAL_PEER_CONTROL_QUEUE_MAX_BYTES)).toBe(true);
		expect(historyQuota.reserve(1)).toBe(false);
		expect(controlQuota.reserve(1)).toBe(false);
		controlQuota.release(TERMINAL_PEER_CONTROL_QUEUE_MAX_BYTES);
		historyQuota.release(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES);
		const workerBudget = new TerminalPeerPacketBudget();
		const firstWorkerPeer = workerBudget.createPeerBudget().quota("outgoing", "history");
		const secondWorkerPeer = workerBudget.createPeerBudget().quota("outgoing", "history");
		const overflowWorkerPeer = workerBudget.createPeerBudget().quota("outgoing", "history");
		expect(firstWorkerPeer.reserve(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES)).toBe(true);
		expect(secondWorkerPeer.reserve(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES)).toBe(true);
		expect(overflowWorkerPeer.reserve(1)).toBe(false);
		firstWorkerPeer.release(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES);
		secondWorkerPeer.release(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES);


		const fixture = createPacketPortFixture();
		for (const lane of ["control", "terminal", "history"] as const) {
			fixture.channels[lane].buffered = TERMINAL_PEER_CHANNEL_WATERMARKS[lane].highBytes;
		}
		expect(fixture.port.send(Uint8Array.of(1), "history")).toBe("backpressured");
		expect(fixture.port.send(Uint8Array.of(2), "terminal")).toBe("backpressured");
		expect(fixture.port.send(Uint8Array.of(3), "control")).toBe("backpressured");
		for (const lane of ["control", "terminal", "history"] as const) {
			fixture.channels[lane].buffered = 0;
		}
		fixture.channels.control.emitLow();
		expect(fixture.sentOrder).toEqual(["control", "terminal", "history"]);
	});


	test("yields before sending more than 64 KiB from one low-water flush", () => {
		const fixture = createPacketPortFixture();
		fixture.channels.control.buffered = TERMINAL_PEER_CHANNEL_WATERMARKS.control.highBytes;
		for (const size of [
			TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES,
			TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES,
			TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES,
			10_000,
			TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES,
		]) {
			fixture.port.send(new Uint8Array(size), "control");
		}
		fixture.channels.control.emitLow();
		const firstTurnBytes = fixture.channels.control.sent.reduce((total, packet) => total + packet.byteLength, 0);
		expect(firstTurnBytes).toBeLessThanOrEqual(TERMINAL_PEER_MAX_FLUSH_BYTES_PER_TURN);
		expect(fixture.channels.control.sent).toHaveLength(4);
		fixture.port.close();
	});

	test("settles a history admission only after its queued final fragment drains", async () => {
		const fixture = createPacketPortFixture();
		fixture.channels.history.buffered = TERMINAL_PEER_CHANNEL_WATERMARKS.history.highBytes;
		expect(fixture.port.send(Uint8Array.of(9), "history")).toBe("backpressured");
		const drained = fixture.port.waitForLaneDrain("history");
		fixture.channels.history.emitLow();
		await drained;
		expect(fixture.channels.history.sent).toHaveLength(1);
	});
	test("transfers a pre-read history ceiling into exact queue ownership", async () => {
		const fixture = createPacketPortFixture();
		fixture.channels.history.buffered = TERMINAL_PEER_CHANNEL_WATERMARKS.history.highBytes;
		const reservationBytes = TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES - 4 * 1024;
		const reservation = fixture.port.reserveHistoryRead(reservationBytes);
		if (!reservation) throw new Error("history pre-read reservation failed");
		expect(fixture.budget.snapshot("outgoing").applicationBytes).toBe(reservationBytes);
		expect(fixture.port.send(Uint8Array.of(7), "history")).toBe("backpressured");
		expect(fixture.budget.snapshot("outgoing").applicationBytes).toBe(reservationBytes + 1);
		reservation.transfer();
		expect(fixture.port.send(Uint8Array.of(9), "history")).toBe("backpressured");
		expect(fixture.budget.snapshot("outgoing").applicationBytes).toBe(2);
		reservation.release();
		expect(fixture.budget.snapshot("outgoing").applicationBytes).toBe(2);
		fixture.channels.history.emitLow();
		await fixture.port.waitForLaneDrain("history");
		expect(fixture.budget.snapshot("outgoing").applicationBytes).toBe(0);
		const orphan = createPacketPortFixture();
		const orphanReservation = orphan.port.reserveHistoryRead(reservationBytes);
		if (!orphanReservation) throw new Error("orphan history reservation failed");
		orphan.port.close();
		expect(orphan.budget.snapshot("outgoing").applicationBytes).toBe(reservationBytes);
		orphanReservation.release();
		expect(orphan.budget.snapshot("outgoing").applicationBytes).toBe(0);
	});
	test("retires a history holder instead of the healthy terminal sender under worker pressure", () => {
		const workerBudget = new TerminalPeerPacketBudget();
		const firstHistory = createPacketPortFixture(workerBudget);
		const secondHistory = createPacketPortFixture(workerBudget);
		const healthyTerminal = createPacketPortFixture(workerBudget);
		const firstReservation = firstHistory.port.reserveHistoryRead(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES);
		const secondReservation = secondHistory.port.reserveHistoryRead(TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES);
		if (!firstReservation || !secondReservation) throw new Error("history reservations failed");

		expect(healthyTerminal.port.send(new Uint8Array(16 * 1024), "terminal")).toBe("accepted");
		expect(healthyTerminal.port.open).toBe(true);
		expect(firstHistory.port.open).toBe(false);
		expect(firstHistory.closeReasons).toContain("application_pressure");
		expect(secondHistory.port.open).toBe(true);

		firstReservation.release();
		secondReservation.release();
		secondHistory.port.close();
		healthyTerminal.port.close();
	});


	test("refuses non-control client data before it can reach a terminal ingress", () => {
		const fixture = createPacketPortFixture();
		fixture.channels.terminal.emitMessage(framedControl(1, Uint8Array.of(1)));
		expect(fixture.port.open).toBe(false);
		expect(fixture.messages).toEqual([]);
		expect(fixture.closeReasons).toEqual(["unexpected_client_data"]);
	});
});
