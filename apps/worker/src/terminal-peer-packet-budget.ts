// Retained-byte accounting for one worker's direct peer packet ports.
// Each direction has independent per-peer and worker ceilings, so slow receive
// assembly cannot spend the outgoing queue budget or starve control replies.

import {
	TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES,
	TERMINAL_PEER_CONTROL_QUEUE_MAX_BYTES,
	TERMINAL_PEER_WORKER_APPLICATION_QUEUE_MAX_BYTES,
	TERMINAL_PEER_WORKER_CONTROL_QUEUE_MAX_BYTES,
	TERMINAL_PEER_WORKER_QUEUE_MAX_BYTES,
	type TerminalPeerPacketLane,
} from "@roost/shared/terminal-peer";
import type { TerminalPeerPacketQuota } from "@roost/shared/terminal-peer-packets";

export type TerminalPeerPacketDirection = "incoming" | "outgoing";

type LaneRetainedBytes = Record<TerminalPeerPacketLane, number>;

export interface TerminalPeerPacketBudgetSnapshot {
	readonly applicationBytes: number;
	readonly controlBytes: number;
	readonly retainedBytes: number;
}
type TerminalPeerHistoryPressureHandler = () => boolean;

class TerminalPeerPacketDirectionBudget {
	private applicationBytes = 0;
	private controlBytes = 0;

	reserve(lane: TerminalPeerPacketLane, bytes: number): boolean {
		if (!isPositiveByteCount(bytes)) return false;
		if (lane === "control") {
			if (this.controlBytes + bytes > TERMINAL_PEER_WORKER_CONTROL_QUEUE_MAX_BYTES) return false;
		} else if (this.applicationBytes + bytes > TERMINAL_PEER_WORKER_APPLICATION_QUEUE_MAX_BYTES) {
			return false;
		}
		if (this.applicationBytes + this.controlBytes + bytes > TERMINAL_PEER_WORKER_QUEUE_MAX_BYTES) {
			return false;
		}
		if (lane === "control") this.controlBytes += bytes;
		else this.applicationBytes += bytes;
		return true;
	}

	release(lane: TerminalPeerPacketLane, bytes: number): void {
		if (!isPositiveByteCount(bytes)) throw new Error("terminal peer packet release is invalid");
		if (lane === "control") {
			if (bytes > this.controlBytes) throw new Error("terminal peer control budget underflow");
			this.controlBytes -= bytes;
			return;
		}
		if (bytes > this.applicationBytes) throw new Error("terminal peer application budget underflow");
		this.applicationBytes -= bytes;
	}

	snapshot(): TerminalPeerPacketBudgetSnapshot {
		return {
			applicationBytes: this.applicationBytes,
			controlBytes: this.controlBytes,
			retainedBytes: this.applicationBytes + this.controlBytes,
		};
	}
}

/** Worker-owned aggregate accounting. Create exactly one peer budget per peer port. */
export class TerminalPeerPacketBudget {
	private readonly directions: Record<TerminalPeerPacketDirection, TerminalPeerPacketDirectionBudget> = {
		incoming: new TerminalPeerPacketDirectionBudget(),
		outgoing: new TerminalPeerPacketDirectionBudget(),
	};
	private readonly historyPressureHandlers = new Set<TerminalPeerHistoryPressureHandler>();
	private disposed = false;

	createPeerBudget(): TerminalPeerPacketPeerBudget {
		if (this.disposed) throw new Error("terminal peer packet budget is disposed");
		return new TerminalPeerPacketPeerBudget(this);
	}

	registerHistoryPressureHandler(handler: TerminalPeerHistoryPressureHandler): () => void {
		this.historyPressureHandlers.add(handler);
		return () => { this.historyPressureHandlers.delete(handler); };
	}

	snapshot(direction: TerminalPeerPacketDirection): TerminalPeerPacketBudgetSnapshot {
		return this.directions[direction].snapshot();
	}

	dispose(): void {
		this.disposed = true;
		this.historyPressureHandlers.clear();
	}
	reserve(
		direction: TerminalPeerPacketDirection,
		lane: TerminalPeerPacketLane,
		bytes: number,
		excludedPressureHandler?: TerminalPeerHistoryPressureHandler,
	): boolean {
		if (this.disposed) return false;
		if (this.directions[direction].reserve(lane, bytes)) return true;
		if (direction !== "outgoing" || lane !== "terminal") return false;
		for (const relievePressure of [...this.historyPressureHandlers]) {
			if (relievePressure === excludedPressureHandler) continue;
			if (relievePressure() && this.directions[direction].reserve(lane, bytes)) return true;
		}
		return false;
	}

	release(direction: TerminalPeerPacketDirection, lane: TerminalPeerPacketLane, bytes: number): void {
		this.directions[direction].release(lane, bytes);
	}
}

/** One peer's two-direction reservations; queues and assemblers receive lane quotas from here. */
export class TerminalPeerPacketPeerBudget {
	private readonly retained: Record<TerminalPeerPacketDirection, LaneRetainedBytes> = {
		incoming: { control: 0, terminal: 0, history: 0 },
		outgoing: { control: 0, terminal: 0, history: 0 },
	};
	private disposed = false;
	private disposeRequested = false;
	private holds = 0;
	private pressureHandler: TerminalPeerHistoryPressureHandler | null = null;

	constructor(private readonly workerBudget: TerminalPeerPacketBudget) {}

	quota(direction: TerminalPeerPacketDirection, lane: TerminalPeerPacketLane): TerminalPeerPacketQuota {
		return {
			reserve: (bytes) => this.reserve(direction, lane, bytes),
			release: (bytes) => this.release(direction, lane, bytes),
		};
	}

	registerHistoryPressureHandler(handler: TerminalPeerHistoryPressureHandler): () => void {
		this.pressureHandler = handler;
		const unregister = this.workerBudget.registerHistoryPressureHandler(handler);
		return () => {
			unregister();
			if (this.pressureHandler === handler) this.pressureHandler = null;
		};
	}

	/** Keeps worker accounting charged while async work outlives a closing port. */
	hold(): () => void {
		if (this.disposeRequested || this.disposed) throw new Error("terminal peer packet budget is closing");
		this.holds += 1;
		let released = false;
		return () => {
			if (released) return;
			released = true;
			this.holds -= 1;
			this.finishDispose();
		};
	}

	retainedBytes(direction: TerminalPeerPacketDirection): number {
		const retained = this.retained[direction];
		return retained.control + retained.terminal + retained.history;
	}

	snapshot(direction: TerminalPeerPacketDirection): TerminalPeerPacketBudgetSnapshot {
		const retained = this.retained[direction];
		const applicationBytes = retained.terminal + retained.history;
		return {
			applicationBytes,
			controlBytes: retained.control,
			retainedBytes: applicationBytes + retained.control,
		};
	}

	dispose(): void {
		if (this.disposeRequested || this.disposed) return;
		this.disposeRequested = true;
		this.finishDispose();
	}

	private reserve(
		direction: TerminalPeerPacketDirection,
		lane: TerminalPeerPacketLane,
		bytes: number,
	): boolean {
		if (this.disposeRequested || this.disposed || !isPositiveByteCount(bytes)) return false;
		const retained = this.retained[direction];
		const peerApplicationBytes = retained.terminal + retained.history;
		if (lane === "control") {
			if (retained.control + bytes > TERMINAL_PEER_CONTROL_QUEUE_MAX_BYTES) return false;
		} else if (peerApplicationBytes + bytes > TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES) {
			return false;
		}
		if (peerApplicationBytes + retained.control + bytes >
			TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES + TERMINAL_PEER_CONTROL_QUEUE_MAX_BYTES) {
			return false;
		}
		if (!this.workerBudget.reserve(direction, lane, bytes, this.pressureHandler ?? undefined)) return false;
		if (this.disposeRequested || this.disposed) {
			this.workerBudget.release(direction, lane, bytes);
			return false;
		}
		retained[lane] += bytes;
		return true;
	}

	private release(
		direction: TerminalPeerPacketDirection,
		lane: TerminalPeerPacketLane,
		bytes: number,
	): void {
		if (this.disposed) return;
		if (!isPositiveByteCount(bytes) || bytes > this.retained[direction][lane]) {
			throw new Error("terminal peer packet peer budget underflow");
		}
		this.retained[direction][lane] -= bytes;
		this.workerBudget.release(direction, lane, bytes);
	}

	private finishDispose(): void {
		if (!this.disposeRequested || this.disposed || this.holds !== 0) return;
		this.disposed = true;
		for (const direction of ["incoming", "outgoing"] as const) {
			for (const lane of ["control", "terminal", "history"] as const) {
				const bytes = this.retained[direction][lane];
				if (bytes === 0) continue;
				this.retained[direction][lane] = 0;
				this.workerBudget.release(direction, lane, bytes);
			}
		}
	}
}

function isPositiveByteCount(bytes: number): boolean {
	return Number.isSafeInteger(bytes) && bytes > 0;
}
