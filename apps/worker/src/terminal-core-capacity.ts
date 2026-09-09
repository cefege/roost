// TerminalCoreCapacity owns worker-local WTerm admission and replacement headroom.
// SessionManager holds each returned lease from core construction through record teardown.
// Boot keeper admission uses the same owner to reject an over-cap survivor before mutation.
// Heartbeat callers snapshot this content-free operational state.

import { signal } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import { totalmem } from "node:os";
import { effectiveLinuxMemoryCeilingBytes } from "./host-sample-linux.ts";

export const TERMINAL_CORE_CAPACITY_HARD_MAX = 500;

export const TERMINAL_CORE_ALLOCATION_BYTES = 40 * 1024 * 1024;
export const TERMINAL_CORE_CAPACITY_ERROR_CODE = "terminal_core_capacity";
export const TERMINAL_CORE_CAPACITY_ERROR_MESSAGE = "terminal core capacity exhausted";

export type TerminalCoreAllocationKind = "fresh" | "adoption" | "replacement";
export type TerminalCoreCapacityRefusalReason =
	| TerminalCoreAllocationKind
	| "survivor_count";

export interface TerminalCoreCapacitySnapshot {
	used: number;
	pending: number;
	capacity: number;
	estimatedReservedBytes: number;
	effectiveMemoryCeilingBytes: number;
	bootRssBytes: number;
	overcommitCount: number;
	refusalCount: number;
}

export interface TerminalCoreLease {
	readonly allocationKind: TerminalCoreAllocationKind;
	activate(): void;
	release(): void;
}

export interface TerminalCoreCapacityOptions {
	effectiveMemoryCeilingBytes: number;
	bootRssBytes: number;
	terminalCoreCap?: number;
}

export interface WorkerTerminalCoreCapacityOptions {
	terminalCoreCap?: number;
	platform?: NodeJS.Platform;
	hostMemoryBytes?: number;
	bootRssBytes?: number;
}

/** Stable admission error so the worker can stop safely without mistaking an
 * intentional capacity refusal for a keeper or terminal-core fault. */
export class TerminalCoreCapacityError extends Error {
	readonly code = TERMINAL_CORE_CAPACITY_ERROR_CODE;
	readonly reason: TerminalCoreCapacityRefusalReason;

	constructor(reason: TerminalCoreCapacityRefusalReason) {
		super(TERMINAL_CORE_CAPACITY_ERROR_MESSAGE);
		this.name = "TerminalCoreCapacityError";
		this.reason = reason;
	}
}

export function isTerminalCoreCapacityError(
	error: unknown,
): error is TerminalCoreCapacityError {
	if (error instanceof TerminalCoreCapacityError) return true;
	return typeof error === "object"
		&& error !== null
		&& "code" in error
		&& error.code === TERMINAL_CORE_CAPACITY_ERROR_CODE;
}

/** Calculate the host-derived steady-state cap before a configured upper bound. */
export function defaultTerminalCoreCapacity(
	effectiveMemoryCeilingBytes: number,
	bootRssBytes: number,
): number {
	const ceiling = checkedBytes(
		effectiveMemoryCeilingBytes,
		"effective memory ceiling",
	);
	const bootRss = checkedBytes(bootRssBytes, "boot RSS");
	const seventyPercentCeiling =
		Math.floor(ceiling / 10) * 7
		+ Math.floor((ceiling % 10) * 7 / 10);
	const allocatable = Math.max(
		0,
		seventyPercentCeiling - bootRss - TERMINAL_CORE_ALLOCATION_BYTES,
	);
	return Math.min(
		TERMINAL_CORE_CAPACITY_HARD_MAX,
		Math.max(0, Math.floor(allocatable / TERMINAL_CORE_ALLOCATION_BYTES)),
	);
}

/** Construct the worker-owned capacity state from the current host once at boot. */
export function createWorkerTerminalCoreCapacity(
	options: WorkerTerminalCoreCapacityOptions = {},
): TerminalCoreCapacity {
	const platform = options.platform ?? process.platform;
	const hostMemoryBytes = nonnegativeBytes(options.hostMemoryBytes ?? totalmem());
	const effectiveMemoryCeilingBytes = platform === "linux"
		? effectiveLinuxMemoryCeilingBytes(hostMemoryBytes)
		: hostMemoryBytes;
	const bootRssBytes = nonnegativeBytes(
		options.bootRssBytes ?? process.memoryUsage().rss,
	);
	return new TerminalCoreCapacity({
		effectiveMemoryCeilingBytes,
		bootRssBytes,
		terminalCoreCap: options.terminalCoreCap,
	});
}

/** One owner tracks every pending and resident core. Replacement leases retain
 * their serialized slot until their caller has torn down the old record. */
export class TerminalCoreCapacity {
	readonly #capacity: number;
	readonly #effectiveMemoryCeilingBytes: number;
	readonly #bootRssBytes: number;
	readonly #pending = new Set<TerminalCoreLeaseImpl>();
	readonly #used = new Set<TerminalCoreLeaseImpl>();
	#replacementLease: TerminalCoreLeaseImpl | null = null;
	#refusalCount = 0;

	constructor(options: TerminalCoreCapacityOptions) {
		this.#effectiveMemoryCeilingBytes = checkedBytes(
			options.effectiveMemoryCeilingBytes,
			"effective memory ceiling",
		);
		this.#bootRssBytes = checkedBytes(options.bootRssBytes, "boot RSS");
		const defaultCapacity = defaultTerminalCoreCapacity(
			this.#effectiveMemoryCeilingBytes,
			this.#bootRssBytes,
		);
		this.#capacity = options.terminalCoreCap === undefined
			? defaultCapacity
			: Math.min(
				defaultCapacity,
				checkedCount(options.terminalCoreCap, "terminal core cap"),
			);
		log.info("terminal-core-capacity", "terminal_core_capacity_initialized", {
			capacity: this.#capacity,
			effective_memory_ceiling_bytes: this.#effectiveMemoryCeilingBytes,
			boot_rss_bytes: this.#bootRssBytes,
		});
	}

	snapshot(): TerminalCoreCapacitySnapshot {
		const used = this.#used.size;
		const pending = this.#pending.size;
		return {
			used,
			pending,
			capacity: this.#capacity,
			estimatedReservedBytes: (used + pending) * TERMINAL_CORE_ALLOCATION_BYTES,
			effectiveMemoryCeilingBytes: this.#effectiveMemoryCeilingBytes,
			bootRssBytes: this.#bootRssBytes,
			overcommitCount: Math.max(0, used + pending - this.#capacity),
			refusalCount: this.#refusalCount,
		};
	}

	reserveFresh(): TerminalCoreLease {
		return this.#reserveSteady("fresh");
	}

	reserveAdoption(): TerminalCoreLease {
		return this.#reserveSteady("adoption");
	}

	reserveReplacement(): TerminalCoreLease {
		if (
			this.#capacity === 0
			|| this.#replacementLease !== null
			|| this.#leasedCount() >= this.#capacity + 1
		) {
			return this.#refuse("replacement");
		}
		const lease = this.#newLease("replacement");
		this.#replacementLease = lease;
		return lease;
	}

	/** Missing binding proof cannot be treated as an empty survivor set. */
	refuseUnknownSurvivorInventory(): never {
		return this.#refuse("survivor_count");
	}

	/** Reject a complete keeper survivor set before any channel is attached or
	 * a partial set of cores could be allocated. */
	assertCanAdoptSurvivors(channelCount: number): void {
		const count = checkedCount(channelCount, "survivor channel count");
		if (this.#leasedCount() + count > this.#capacity) {
			this.#refuse("survivor_count");
		}
	}

	/** The respawn caller invokes this only after `_dropChannelState` released
	 * the old record. It frees serialization, not the new record's live lease. */
	completeReplacement(lease: TerminalCoreLease): void {
		if (!(lease instanceof TerminalCoreLeaseImpl)
			|| lease.owner !== this
			|| this.#replacementLease !== lease
			|| !this.#used.has(lease)) {
			throw new Error("terminal core replacement completion is invalid");
		}
		this.#replacementLease = null;
		this.#logTransition("terminal_core_replacement_completed", lease.allocationKind);
	}

	#reserveSteady(kind: "fresh" | "adoption"): TerminalCoreLease {
		if (this.#capacity === 0 || this.#leasedCount() >= this.#capacity) {
			return this.#refuse(kind);
		}
		return this.#newLease(kind);
	}

	#newLease(allocationKind: TerminalCoreAllocationKind): TerminalCoreLeaseImpl {
		const lease = new TerminalCoreLeaseImpl(this, allocationKind);
		this.#pending.add(lease);
		this.#logTransition("terminal_core_capacity_reserved", allocationKind);
		return lease;
	}

	#activate(lease: TerminalCoreLeaseImpl): void {
		if (!this.#pending.delete(lease)) {
			throw new Error("terminal core lease cannot activate twice or after release");
		}
		this.#used.add(lease);
		this.#logTransition("terminal_core_capacity_activated", lease.allocationKind);
	}

	#release(lease: TerminalCoreLeaseImpl): void {
		const released = this.#pending.delete(lease) || this.#used.delete(lease);
		if (!released) return;
		if (this.#replacementLease === lease) this.#replacementLease = null;
		this.#logTransition("terminal_core_capacity_released", lease.allocationKind);
	}

	#refuse(reason: TerminalCoreCapacityRefusalReason): never {
		if (this.#refusalCount < Number.MAX_SAFE_INTEGER) this.#refusalCount += 1;
		const snapshot = this.snapshot();
		const fields = {
			reason,
			refusal_count: snapshot.refusalCount,
			capacity: snapshot.capacity,
			used: snapshot.used,
			pending: snapshot.pending,
		};
		signal("terminal.core_capacity", { ...fields, cooldownKey: "worker" });
		throw new TerminalCoreCapacityError(reason);
	}

	#leasedCount(): number {
		return this.#used.size + this.#pending.size;
	}

	#logTransition(event: string, allocationKind: TerminalCoreAllocationKind): void {
		const snapshot = this.snapshot();
		log.debug("terminal-core-capacity", event, {
			allocation_kind: allocationKind,
			capacity: snapshot.capacity,
			used: snapshot.used,
			pending: snapshot.pending,
			overcommit_count: snapshot.overcommitCount,
		});
	}

	/** Lease methods are intentionally only reachable from the object that owns
	 * a core, preventing callers from altering counters directly. */
	_activateLease(lease: TerminalCoreLeaseImpl): void {
		this.#activate(lease);
	}

	_releaseLease(lease: TerminalCoreLeaseImpl): void {
		this.#release(lease);
	}
}

class TerminalCoreLeaseImpl implements TerminalCoreLease {
	constructor(
		readonly owner: TerminalCoreCapacity,
		readonly allocationKind: TerminalCoreAllocationKind,
	) {}

	activate(): void {
		this.owner._activateLease(this);
	}

	release(): void {
		this.owner._releaseLease(this);
	}
}

function checkedBytes(value: number, label: string): number {
	if (!Number.isSafeInteger(value) || value < 0) {
		throw new RangeError(`${label} must be a nonnegative safe integer`);
	}
	return value;
}

function nonnegativeBytes(value: number): number {
	return Number.isSafeInteger(value) && value >= 0 ? value : 0;
}

function checkedCount(value: number, label: string): number {
	if (!Number.isSafeInteger(value) || value < 0) {
		throw new RangeError(`${label} must be a nonnegative safe integer`);
	}
	return value;
}
