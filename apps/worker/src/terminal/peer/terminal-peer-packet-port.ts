// WebRTC implementation of the common terminal packet port contract.
// It owns native channel callbacks, framed FIFO queues, reassembly and retained
// packet bytes; LocalTerminalSockets remains the only LocalTerminal frame owner.

import {
	TERMINAL_PEER_CHANNEL_WATERMARKS,
	TERMINAL_PEER_LANE_PRIORITY,
	TERMINAL_PEER_NEGOTIATION_DEADLINE_MS,
	TERMINAL_PEER_MAX_FLUSH_BYTES_PER_TURN,
	TERMINAL_PEER_PACKET_STALL_MS,
	TERMINAL_PEER_UNAUTHENTICATED_CONTROL_MAX_BYTES,
	type TerminalPeerPacketLane,
} from "@roost/protocol/terminal-peer";
import {
	parseTerminalPeerPacket,
	TerminalPeerPacketAssembler,
	TerminalPeerPacketQueue,
} from "@roost/protocol/terminal-peer-packets";
import type { TerminalPacketPort, TerminalPacketSendResult } from "./terminal-packet-port.ts";
import {
	TerminalPeerHistoryReservationOwner,
	type TerminalPeerHistoryReadReservation,
} from "./terminal-peer-history-reservation.ts";
import type { TerminalPeerPacketPeerBudget } from "./terminal-peer-packet-budget.ts";
import {
	malformedTerminalPeerControlPacket,
	type TerminalPeerMalformedPacketKind,
} from "./terminal-peer-packet-test-fault.ts";

export interface TerminalPeerNativeDataChannel {
	close(): void;
	sendMessageBinary(bytes: Buffer | Uint8Array): boolean;
	isOpen(): boolean;
	bufferedAmount(): number;
	setBufferedAmountLowThreshold(bytes: number): void;
	onOpen(callback: () => void): void;
	onClosed(callback: () => void): void;
	onError(callback: (error: string) => void): void;
	onBufferedAmountLow(callback: () => void): void;
	onMessage(callback: (message: string | Uint8Array | ArrayBuffer) => void): void;
}
export type TerminalPeerNativeDataChannels = Record<TerminalPeerPacketLane, TerminalPeerNativeDataChannel>;
export interface TerminalPeerPacketIngress {
	onMessage(bytes: Uint8Array): void;
	onClose?(): void;
}
export interface TerminalPeerPacketPortDeps {
	readonly socketId: string;
	readonly channels: TerminalPeerNativeDataChannels;
	readonly packetBudget: TerminalPeerPacketPeerBudget;
	readonly onChannelOpen?: () => void;
	readonly onClosed?: (reason: string) => void;
	readonly onFatal?: (reason: string) => void;
	readonly now?: () => number;
	/** Source-smoke-only outgoing packet blackhole after queue ownership. */
	readonly shouldBlackholeOutgoing?: () => boolean;
}
interface LaneDrainWaiter {
	resolve(): void;
	reject(error: Error): void;
}


/** One native PeerConnection's framed direct terminal carrier. */
export class TerminalPeerPacketPort implements TerminalPacketPort {
	readonly kind = "webrtc" as const;
	readonly socketId: string;
	private readonly queues: Record<TerminalPeerPacketLane, TerminalPeerPacketQueue>;
	private readonly assemblers: Record<TerminalPeerPacketLane, TerminalPeerPacketAssembler>;
	private readonly historyReservations: TerminalPeerHistoryReservationOwner;
	private readonly unregisterHistoryPressure: () => void;
	private readonly now: () => number;
	private ingress: TerminalPeerPacketIngress | null = null;
	private authenticated = false;
	private closed = false;
	private flushing = false;
	private flushTimer: NodeJS.Timeout | undefined;
	private readonly partialTimers: Record<TerminalPeerPacketLane, NodeJS.Timeout | undefined> = {
		control: undefined,
		terminal: undefined,
		history: undefined,
	};
	private setupTimer: NodeJS.Timeout | undefined;
	private historyDeliveryPausedForTest = false;
	private readonly backpressured: Record<TerminalPeerPacketLane, boolean> = {
		control: false,
		terminal: false,
		history: false,
	};
	private readonly laneDrainWaiters: Record<TerminalPeerPacketLane, Set<LaneDrainWaiter>> = {
		control: new Set(),
		terminal: new Set(),
		history: new Set(),
	};

	constructor(private readonly deps: TerminalPeerPacketPortDeps) {
		this.socketId = deps.socketId;
		this.now = deps.now ?? (() => performance.now());
		this.historyReservations = new TerminalPeerHistoryReservationOwner(
			deps.packetBudget.quota("outgoing", "history"),
			() => deps.packetBudget.hold(),
		);
		this.queues = {
			control: new TerminalPeerPacketQueue("control", deps.packetBudget.quota("outgoing", "control")),
			terminal: new TerminalPeerPacketQueue("terminal", deps.packetBudget.quota("outgoing", "terminal")),
			history: new TerminalPeerPacketQueue("history", this.historyReservations.queueQuota),
		};
		this.unregisterHistoryPressure = deps.packetBudget.registerHistoryPressureHandler(
			() => this.relieveHistoryPressure(),
		);
		this.assemblers = {
			control: new TerminalPeerPacketAssembler("control", deps.packetBudget.quota("incoming", "control"), this.now),
			terminal: new TerminalPeerPacketAssembler("terminal", deps.packetBudget.quota("incoming", "terminal"), this.now),
			history: new TerminalPeerPacketAssembler("history", deps.packetBudget.quota("incoming", "history"), this.now),
		};
		for (const lane of TERMINAL_PEER_LANE_PRIORITY) this.installChannelCallbacks(lane);
		this.setupTimer = setTimeout(() => {
			this.setupTimer = undefined;
			if (!this.authenticated) this.fail("setup_timeout");
		}, TERMINAL_PEER_NEGOTIATION_DEADLINE_MS);
		this.setupTimer.unref?.();
	}

	get open(): boolean {
		return !this.closed && this.deps.channels.control.isOpen();
	}

	/** The injected LocalTerminalSockets adapter binds before remote SDP can deliver a frame. */
	attachIngress(ingress: TerminalPeerPacketIngress): void {
		if (this.closed || this.ingress !== null) throw new Error("terminal peer packet ingress is unavailable");
		this.ingress = ingress;
	}

	/** Concrete-only authentication seam: common packet ports do not expose hello state. */
	markAuthenticated(): void {
		if (this.closed || this.ingress === null) return;
		this.authenticated = true;
		clearTimeout(this.setupTimer);
		this.setupTimer = undefined;
	}

	injectMalformedPacketForTest(kind: TerminalPeerMalformedPacketKind): boolean {
		if (this.closed || !this.authenticated) return false;
		this.receive("control", malformedTerminalPeerControlPacket(kind));
		return true;
	}

	setHistoryDeliveryPausedForTest(paused: boolean): void {
		if (this.closed) return;
		this.historyDeliveryPausedForTest = paused;
		if (!paused) this.flush();
	}

	bufferedBytes(): number {
		let bytes = 0;
		for (const lane of TERMINAL_PEER_LANE_PRIORITY) {
			bytes += this.queues[lane].queuedBytes;
			try {
				const buffered = this.deps.channels[lane].bufferedAmount();
				if (Number.isSafeInteger(buffered) && buffered > 0) bytes += buffered;
			} catch {
				return Number.MAX_SAFE_INTEGER;
			}
		}
		return bytes;
	}

	/** Resolves when this port no longer owns a complete queued message on the lane. */
	waitForLaneDrain(lane: TerminalPeerPacketLane): Promise<void> {
		if (this.closed) return Promise.reject(new Error("terminal peer packet port is closed"));
		if (this.queues[lane].messageCount === 0) return Promise.resolve();
		return new Promise<void>((resolve, reject) => {
			this.laneDrainWaiters[lane].add({ resolve, reject });
		});
	}

	/** Reserves the complete direct-history ceiling before the sliced reader allocates rows. */
	reserveHistoryRead(bytes: number): TerminalPeerHistoryReadReservation | null {
		return this.historyReservations.reserve(bytes);
	}

	send(bytes: Uint8Array, lane: TerminalPeerPacketLane): TerminalPacketSendResult {
		if (this.closed) return "refused";
		const queue = this.queues[lane];
		const channel = this.deps.channels[lane];
		this.backpressured[lane] = false;
		const queuedBefore = queue.queuedBytes;
		try {
			if (!queue.enqueue(bytes)) return "refused";
		} catch {
			return "refused";
		}
		const wasBackpressured = !channel.isOpen() || this.isFlowControlled(lane) || queuedBefore > 0;
		this.flush();
		if (this.closed) return "refused";
		const result = wasBackpressured || this.backpressured[lane] || this.isFlowControlled(lane)
			? "backpressured"
			: "accepted";
		this.backpressured[lane] = false;
		return result;
	}

	close(_code?: number, reason = "terminal peer port closed"): void {
		if (this.closed) return;
		this.closed = true;
		this.unregisterHistoryPressure();
		clearTimeout(this.flushTimer);
		clearTimeout(this.setupTimer);
		this.flushTimer = undefined;
		this.setupTimer = undefined;
		for (const lane of TERMINAL_PEER_LANE_PRIORITY) {
			clearTimeout(this.partialTimers[lane]);
			this.partialTimers[lane] = undefined;
			try { this.queues[lane].clear(); } catch { /* quota cleanup remains local to this peer */ }
			try { this.assemblers[lane].reset(); } catch { /* malformed peers are already closing */ }
			try { this.deps.channels[lane].close(); } catch { /* native channel already closed */ }
		}
		this.rejectLaneDrainWaiters(reason);
		this.deps.packetBudget.dispose();
		try { this.ingress?.onClose?.(); } catch { /* socket teardown cannot escape native callbacks */ }
		this.deps.onClosed?.(reason);
	}

	private installChannelCallbacks(lane: TerminalPeerPacketLane): void {
		const channel = this.deps.channels[lane];
		const watermarks = TERMINAL_PEER_CHANNEL_WATERMARKS[lane];
		channel.setBufferedAmountLowThreshold(watermarks.lowBytes);
		channel.onOpen(() => {
			if (this.closed) return;
			this.deps.onChannelOpen?.();
			this.flush();
		});
		channel.onBufferedAmountLow(() => {
			if (this.closed) return;
			this.backpressured[lane] = false;
			this.flush();
		});
		channel.onMessage((message) => { this.receive(lane, message); });
		channel.onError(() => { this.fail("data_channel_error"); });
		channel.onClosed(() => { this.fail("data_channel_closed"); });
	}

	private receive(lane: TerminalPeerPacketLane, message: string | Uint8Array | ArrayBuffer): void {
		if (this.closed) return;
		if (lane !== "control" || typeof message === "string" || this.ingress === null) {
			this.fail("unexpected_client_data");
			return;
		}
		const bytes = message instanceof ArrayBuffer
			? new Uint8Array(message)
			: new Uint8Array(message.buffer, message.byteOffset, message.byteLength);
		try {
			const header = parseTerminalPeerPacket(lane, bytes);
			if (!this.authenticated && header.totalBytes > TERMINAL_PEER_UNAUTHENTICATED_CONTROL_MAX_BYTES) {
				this.fail("unauthenticated_frame_too_large");
				return;
			}
			const complete = this.assemblers[lane].push(bytes, this.now());
			this.armPartialDeadline(lane);
			if (complete !== null) this.ingress.onMessage(complete);
		} catch {
			this.fail("packet_rejected");
		}
	}

	private armPartialDeadline(lane: TerminalPeerPacketLane): void {
		clearTimeout(this.partialTimers[lane]);
		this.partialTimers[lane] = undefined;
		if (!this.assemblers[lane].hasPartialMessage) return;
		this.partialTimers[lane] = setTimeout(() => {
			this.partialTimers[lane] = undefined;
			try {
				if (this.assemblers[lane].expire(this.now())) this.fail("packet_stalled");
			} catch {
				this.fail("packet_stalled");
			}
		}, TERMINAL_PEER_PACKET_STALL_MS);
		this.partialTimers[lane]?.unref?.();
	}


	private flush(): void {
		if (this.closed || this.flushing) return;
		this.flushing = true;
		let sentBytes = 0;
		let shouldYield = false;
		try {
			for (;;) {
				let progressed = false;
				for (const lane of TERMINAL_PEER_LANE_PRIORITY) {
					if (lane === "history" && this.historyDeliveryPausedForTest) continue;
					if (this.isFlowControlled(lane) || !this.deps.channels[lane].isOpen()) continue;
					const fragment = this.queues[lane].nextFragment();
					if (fragment === null) continue;
					if (
						sentBytes > 0
						&& sentBytes + fragment.bytes.byteLength > TERMINAL_PEER_MAX_FLUSH_BYTES_PER_TURN
					) {
						shouldYield = true;
						break;
					}
					try {
						if (this.authenticated && this.deps.shouldBlackholeOutgoing?.()) {
							fragment.commit();
							this.resolveLaneDrainWaiters(lane);
							sentBytes += fragment.bytes.byteLength;
							progressed = true;
							if (sentBytes >= TERMINAL_PEER_MAX_FLUSH_BYTES_PER_TURN) {
								shouldYield = true;
								break;
							}
							continue;
						}
						// libdatachannel's false reports buffered acceptance, never a refused send.
						const acceptedImmediately = this.deps.channels[lane].sendMessageBinary(
							Buffer.from(fragment.bytes.buffer, fragment.bytes.byteOffset, fragment.bytes.byteLength),
						);
						fragment.commit();
						this.backpressured[lane] = !acceptedImmediately;
						this.resolveLaneDrainWaiters(lane);
					} catch {
						this.fail("native_send_failed");
						return;
					}
					sentBytes += fragment.bytes.byteLength;
					progressed = true;
					if (sentBytes >= TERMINAL_PEER_MAX_FLUSH_BYTES_PER_TURN) {
						shouldYield = true;
						break;
					}
				}
				if (!progressed || shouldYield || this.closed) break;
			}
		} catch {
			this.fail("packet_queue_failed");
		} finally {
			this.flushing = false;
		}
		if (shouldYield && !this.closed) this.scheduleFlush();
	}

	private scheduleFlush(): void {
		if (this.flushTimer !== undefined || !this.hasQueuedMessages()) return;
		this.flushTimer = setTimeout(() => {
			this.flushTimer = undefined;
			this.flush();
		}, 0);
		this.flushTimer.unref?.();
	}


	private resolveLaneDrainWaiters(lane: TerminalPeerPacketLane): void {
		if (this.queues[lane].messageCount !== 0) return;
		const waiters = this.laneDrainWaiters[lane];
		for (const waiter of waiters) waiter.resolve();
		waiters.clear();
	}

	private rejectLaneDrainWaiters(reason: string): void {
		const error = new Error(reason);
		for (const lane of TERMINAL_PEER_LANE_PRIORITY) {
			const waiters = this.laneDrainWaiters[lane];
			for (const waiter of waiters) waiter.reject(error);
			waiters.clear();
		}
	}
	private hasQueuedMessages(): boolean {
		return TERMINAL_PEER_LANE_PRIORITY.some((lane) => this.queues[lane].messageCount > 0);
	}

	private isFlowControlled(lane: TerminalPeerPacketLane): boolean {
		const channel = this.deps.channels[lane];
		try {
			return channel.bufferedAmount() >= TERMINAL_PEER_CHANNEL_WATERMARKS[lane].highBytes;
		} catch {
			return true;
		}
	}

	private relieveHistoryPressure(): boolean {
		const heldApplication = this.historyReservations.cancelForPressure()
			|| this.queues.terminal.messageCount > 0
			|| this.queues.history.messageCount > 0;
		if (!heldApplication) return false;
		this.fail("application_pressure");
		return true;
	}


	private fail(reason: string): void {
		if (this.closed) return;
		try { this.deps.onFatal?.(reason); } catch { /* close remains fail-closed if a callback faults */ }
		this.close(undefined, reason);
	}
}
