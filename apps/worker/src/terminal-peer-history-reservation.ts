// Direct-history pre-read quota and atomic queue handoff.
// One read owns the worst-case retained-byte reservation; only that read's
// explicitly transferred final response may replace it with exact queue bytes.

import type { TerminalPeerPacketQuota } from "@roost/protocol/terminal-peer-packets";

export interface TerminalPeerHistoryReadReservation {
	transfer(): void;
	release(): void;
}

export class TerminalPeerHistoryReservationOwner {
	readonly queueQuota: TerminalPeerPacketQuota;
	private bytes = 0;
	private reservationId = 0;
	private transferReservationId: number | null = null;
	private releaseHold: (() => void) | null = null;

	constructor(
		private readonly baseQuota: TerminalPeerPacketQuota,
		private readonly acquireHold: () => () => void,
	) {
		this.queueQuota = {
			reserve: (bytes) => this.reserveQueued(bytes),
			release: (bytes) => this.baseQuota.release(bytes),
		};
	}

	reserve(bytes: number): TerminalPeerHistoryReadReservation | null {
		if (this.bytes !== 0 || !this.baseQuota.reserve(bytes)) return null;
		let releaseHold: () => void;
		try {
			releaseHold = this.acquireHold();
		} catch {
			this.baseQuota.release(bytes);
			return null;
		}
		const reservationId = ++this.reservationId;
		this.bytes = bytes;
		this.releaseHold = releaseHold;
		return {
			transfer: () => {
				if (this.reservationId === reservationId && this.bytes !== 0) {
					this.transferReservationId = reservationId;
				}
			},
			release: () => {
				if (this.reservationId !== reservationId) return;
				if (this.bytes !== 0) {
					const reservedBytes = this.bytes;
					this.bytes = 0;
					this.transferReservationId = null;
					this.baseQuota.release(reservedBytes);
				}
				this.releaseHold?.();
				this.releaseHold = null;
			},
		};
	}

	cancelForPressure(): boolean {
		if (this.bytes === 0) return false;
		const reservedBytes = this.bytes;
		this.bytes = 0;
		this.transferReservationId = null;
		this.reservationId += 1;
		this.baseQuota.release(reservedBytes);
		this.releaseHold?.();
		this.releaseHold = null;
		return true;
	}

	private reserveQueued(bytes: number): boolean {
		if (this.bytes !== 0 && this.transferReservationId === this.reservationId) {
			const reservedBytes = this.bytes;
			this.bytes = 0;
			this.transferReservationId = null;
			this.baseQuota.release(reservedBytes);
		}
		return this.baseQuota.reserve(bytes);
	}
}
