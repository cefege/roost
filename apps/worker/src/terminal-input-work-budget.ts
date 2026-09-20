// TerminalInputWorkBudget owns the bounded asynchronous work admitted from
// browser terminal ports. CoordLink and direct ports reserve source bytes before
// copying or entering keeper admission, then release exactly once after outcome
// settlement so a slow peer cannot build an unbounded promise chain.

export const TERMINAL_INPUT_WORK_MAX_REQUESTS = 256;
export const TERMINAL_INPUT_WORK_MAX_BYTES = 16 * 1024 * 1024;
export const TERMINAL_DIRECT_INPUT_WORK_MAX_REQUESTS = 32;
export const TERMINAL_DIRECT_INPUT_WORK_MAX_BYTES = 2 * 1024 * 1024;
export const TERMINAL_ROUTE_CLAIM_WORK_MAX_REQUESTS = 32;
export const TERMINAL_DIRECT_ROUTE_CLAIM_WORK_MAX_REQUESTS = 4;

export type TerminalInputWorkOrigin = "sync" | "direct";

export interface TerminalInputWorkRequest {
	origin: TerminalInputWorkOrigin;
	byteLength: number;
	/** Direct ports have a per-port reservation ceiling; Sync does not. */
	portId?: string;
}

export interface TerminalInputWorkReservation {
	release(): void;
}

export class TerminalRouteClaimWorkReservation {
	private released = false;

	constructor(private readonly releaseReservation: () => void) {}

	release(): void {
		if (this.released) return;
		this.released = true;
		this.releaseReservation();
	}
}

export type TerminalInputWorkAdmission =
	| { admitted: true; reservation: TerminalInputWorkReservation }
	| { admitted: false; reason: "worker input admission is full" };

export type TerminalRouteClaimWorkAdmission =
	| { admitted: true; reservation: TerminalRouteClaimWorkReservation }
	| { admitted: false; reason: "route_claim_busy" };

interface DirectPortUsage {
	inputCount: number;
	inputBytes: number;
	claimCount: number;
}

interface InputReservationState {
	active: boolean;
	byteLength: number;
	portId: string | null;
}

interface ClaimReservationState {
	active: boolean;
	portId: string;
	actorSessionKey: string;
}

/**
 * One worker-owned budget for browser-triggered work. This deliberately does
 * not copy or queue bytes: callers retain their one owned copy only after a
 * successful reservation and release the reservation from their result finally.
 */
export class TerminalInputWorkBudget {
	private readonly directPortUsage = new Map<string, DirectPortUsage>();
	private readonly inputReservations = new Set<InputReservationState>();
	private readonly claimReservationStates = new Set<ClaimReservationState>();
	private readonly claimsByActorSession = new Map<string, ClaimReservationState>();
	private inputCount = 0;
	private inputBytes = 0;
	private claimCount = 0;
	private disposed = false;

	reserveInput(request: TerminalInputWorkRequest): TerminalInputWorkAdmission {
		if (!this.canReserveInput(request)) {
			return { admitted: false, reason: "worker input admission is full" };
		}
		const portId = request.origin === "direct" ? request.portId! : null;
		const reservation: InputReservationState = {
			active: true,
			byteLength: request.byteLength,
			portId,
		};
		this.inputReservations.add(reservation);
		this.inputCount += 1;
		this.inputBytes += request.byteLength;
		if (portId !== null) {
			const usage = this.portUsage(portId);
			usage.inputCount += 1;
			usage.inputBytes += request.byteLength;
		}
		return {
			admitted: true,
			reservation: { release: () => this.releaseInput(reservation) },
		};
	}

	reserveRouteClaim(portId: string, actorSessionKey: string): TerminalRouteClaimWorkAdmission {
		if (this.disposed
			|| !validKey(portId)
			|| !validKey(actorSessionKey)
			|| this.claimsByActorSession.has(actorSessionKey)
			|| this.claimCount >= TERMINAL_ROUTE_CLAIM_WORK_MAX_REQUESTS
			|| (this.directPortUsage.get(portId)?.claimCount ?? 0)
				>= TERMINAL_DIRECT_ROUTE_CLAIM_WORK_MAX_REQUESTS) {
			return { admitted: false, reason: "route_claim_busy" };
		}
		return this.createClaimReservation(portId, actorSessionKey);
	}

	dispose(): void {
		if (this.disposed) return;
		this.disposed = true;
		for (const reservation of this.inputReservations) reservation.active = false;
		for (const reservation of this.claimReservationStates) reservation.active = false;
		this.inputReservations.clear();
		this.claimReservationStates.clear();
		this.directPortUsage.clear();
		this.claimsByActorSession.clear();
		this.inputCount = 0;
		this.inputBytes = 0;
		this.claimCount = 0;
	}

	private canReserveInput(request: TerminalInputWorkRequest): boolean {
		if (this.disposed || !Number.isSafeInteger(request.byteLength) || request.byteLength < 0) return false;
		if (request.origin !== "sync" && request.origin !== "direct") return false;
		if (this.inputCount >= TERMINAL_INPUT_WORK_MAX_REQUESTS) return false;
		if (request.byteLength > TERMINAL_INPUT_WORK_MAX_BYTES - this.inputBytes) return false;
		if (request.origin !== "direct") return true;
		if (!validKey(request.portId)) return false;
		const usage = this.directPortUsage.get(request.portId)
			?? { inputCount: 0, inputBytes: 0, claimCount: 0 };
		return usage.inputCount < TERMINAL_DIRECT_INPUT_WORK_MAX_REQUESTS
			&& request.byteLength <= TERMINAL_DIRECT_INPUT_WORK_MAX_BYTES - usage.inputBytes;
	}

	private createClaimReservation(
		portId: string,
		actorSessionKey: string,
	): TerminalRouteClaimWorkAdmission {
		const state: ClaimReservationState = { active: true, portId, actorSessionKey };
		const reservation = new TerminalRouteClaimWorkReservation(() => this.releaseClaim(state));
		this.claimReservationStates.add(state);
		this.claimsByActorSession.set(actorSessionKey, state);
		this.claimCount += 1;
		this.portUsage(portId).claimCount += 1;
		return { admitted: true, reservation };
	}

	private releaseInput(reservation: InputReservationState): void {
		if (!reservation.active) return;
		reservation.active = false;
		this.inputReservations.delete(reservation);
		this.inputCount -= 1;
		this.inputBytes -= reservation.byteLength;
		if (reservation.portId === null) return;
		const usage = this.directPortUsage.get(reservation.portId);
		if (!usage) return;
		usage.inputCount -= 1;
		usage.inputBytes -= reservation.byteLength;
		this.dropUnusedPort(reservation.portId, usage);
	}

	private releaseClaim(reservation: ClaimReservationState): void {
		if (!reservation.active) return;
		reservation.active = false;
		this.claimReservationStates.delete(reservation);
		if (this.claimsByActorSession.get(reservation.actorSessionKey) === reservation) {
			this.claimsByActorSession.delete(reservation.actorSessionKey);
		}
		this.claimCount -= 1;
		const usage = this.directPortUsage.get(reservation.portId);
		if (!usage) return;
		usage.claimCount -= 1;
		this.dropUnusedPort(reservation.portId, usage);
	}

	private portUsage(portId: string): DirectPortUsage {
		let usage = this.directPortUsage.get(portId);
		if (!usage) {
			usage = { inputCount: 0, inputBytes: 0, claimCount: 0 };
			this.directPortUsage.set(portId, usage);
		}
		return usage;
	}

	private dropUnusedPort(portId: string, usage: DirectPortUsage): void {
		if (usage.inputCount === 0 && usage.inputBytes === 0 && usage.claimCount === 0) {
			this.directPortUsage.delete(portId);
		}
	}

}


function validKey(value: string | undefined): value is string {
	return typeof value === "string" && value.length > 0;
}
