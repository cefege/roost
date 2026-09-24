// SessionManagerState owns the mutable maps, transport callbacks and the
// registered cell sinks for terminal sessions. SessionManager extends it with
// lifecycle operations while collaborator modules share the state.
// Keeper maintenance starts only after boot admits the complete coordinator session set.

import type { TerminalControlLane, KeeperAdmissionLane } from "./session-control-lanes.ts";
import type { CellSinkRegistration } from "./session-cell-sinks.ts";
import type { TerminalStreamState } from "./session-terminal-state.ts";
import type {
	CellEmissionSchedule,
	CellGateSuppression,
} from "./session-cell-scheduler.ts";
import type { SyncOutputHold } from "./session-sync-output.ts";
import type { TerminalMetadataState } from "./session-terminal-metadata.ts";
import type {
	TerminalMetadataFrame,
	TransportSendResult,
} from "./transport/coord-link-types.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import { log } from "@roost/observability/log";
import type { TerminalCore } from "@wterm/core";
import type { SessionEventSink } from "./event-sink.ts";
import type { SessionId, WorkerFp } from "@roost/protocol/wire";
import {
	_createWtermCore,
	STRAY_REAP_INTERVAL_MS,
} from "./session-constants.ts";
import type { SessionRecord } from "./session-record.ts";
import {
	createWorkerTerminalCoreCapacity,
	type TerminalCoreAllocationKind,
	type TerminalCoreCapacity,
	type TerminalCoreLease,
} from "./terminal-core-capacity.ts";

export interface AllocatedTerminalCore {
	core: TerminalCore;
	lease: TerminalCoreLease;
}

interface PendingRawMetadataFrame {
	endSeq: number;
	bytes: Uint8Array;
}

interface PendingRawMetadataFrames {
	readonly length: number;
	append(frame: PendingRawMetadataFrame): void;
	peek(): PendingRawMetadataFrame | undefined;
	take(): PendingRawMetadataFrame | undefined;
	clear(): void;
}

type RawMetadataWake =
	| { kind: "microtask" }
	| { kind: "timer"; timer: NodeJS.Timeout };

interface PendingRawMetadataQueue {
	frames: PendingRawMetadataFrames;
	bytes: number;
}

export abstract class SessionManagerState {
	sessions = new Map<number, SessionRecord>();
	_nextChannel = 1;
	// Caller-minted UUID reservation closes the await-before-register window in
	// session-spawn while the worker remains the final collision gate.
	protected readonly pendingSpawnSessionIds = new Set<SessionId>();
	protected pendingSnapshotSessionAdmissions = 0;
	readonly workerFp: WorkerFp;
	readonly sink: SessionEventSink;
	readonly #createTerminalCore: (
		cols: number,
		rows: number,
	) => Promise<TerminalCore>;
	readonly terminalCoreCapacity: TerminalCoreCapacity;
	terminalStreams = new Map<number, TerminalStreamState>();
	protected terminalStreamVersion = 0;
	lastAppliedSize = new Map<number, { cols: number; rows: number }>();
	cellEmitSchedules = new Map<number, CellEmissionSchedule>();
	cellDirty = new Set<number>();
	rawMetadataQueues = new Map<number, PendingRawMetadataQueue>();
	// Insertion order is the round-robin order; Set membership deduplicates it.
	rawMetadataReadyRing = new Set<number>();
	rawMetadataWake: RawMetadataWake | null = null;
	rawMetadataDispatching = false;
	rawMetadataQueuedBytes = 0;
	terminalMetadataByChannel = new Map<number, TerminalMetadataState>();
	terminalMetadataReadyRing = new Set<number>();
	terminalMetadataFlushScheduled = false;
	terminalMetadataFlushToken = 0;
	terminalMetadataFlushing = false;
	terminalMetadataNegotiated = false;
	// Count, not membership: a burst writes several keystrokes before the first
	// return chunk consumes one, and a Set collapses them into one promotion.
	inputSensitiveChannels = new Map<number, number>();
	pendingCellRepairs = new Set<number>();
	pendingSyncCellSnapshots = new Set<number>();
	strayReaperTimer: NodeJS.Timeout | null = null;
	postAdmissionMaintenancePromise: Promise<void> | null = null;
	strayStrikes = new Map<number, number>();
	terminalControlChains = new Map<number, TerminalControlLane>();
	terminalSearches = new Map<string, {
		searchId: string;
		controller: AbortController;
	}>();
	terminalSearchBatches = new Map<string, {
		searchId: string;
		sessionIds: readonly SessionId[];
	}>();
	terminalSearchCancellations = new Map<string, number>();
	keeperAdmissionLane = new Map<number, KeeperAdmissionLane>();
	channelResizeSeq = new Map<number, number>();
	cellEmissionGates = new Set<number>();
	cellGateSuppression = new Map<number, CellGateSuppression>();
	syncOutputHolds = new Map<number, SyncOutputHold>();
	hyperlinkSaturated = new Set<number>();
	readonly sendBinaryUpstream:
		| ((
			channelId: number,
			direction: number,
			endSeq: number,
			bytes: Uint8Array,
		) => TransportSendResult | void)
		| null;
	readonly sendTerminalMetadataUpstream:
		| ((metadata: TerminalMetadataFrame) => TransportSendResult | void)
		| null;
	readonly cellSinks = new Map<string, CellSinkRegistration>();

	_noSessionBurst: number[] = [];
	recentlyClosed = new Map<number, number>();
	_deadBirthBurst: number[] = [];
	onKeeperDegraded: (() => void) | null = null;
	onTerminalChanged: ((channelId: number) => void) | null = null;
	onSessionClosed: ((sessionId: string) => void) | null = null;

	abstract reapStrayKeeperChannels(): Promise<number>;
	startPostAdmissionMaintenance(): Promise<void> {
		if (this.postAdmissionMaintenancePromise) {
			return this.postAdmissionMaintenancePromise;
		}
		const starting = (async () => {
			await getMultiplexedPool().ensure();
			if (this.strayReaperTimer === null) {
				this.strayReaperTimer = setInterval(
					() => void this.reapStrayKeeperChannels(),
					STRAY_REAP_INTERVAL_MS,
				);
			}
			log.info("session-manager", "post_admission_maintenance_started");
		})();
		void starting.catch((error) => {
			if (this.postAdmissionMaintenancePromise === starting) {
				this.postAdmissionMaintenancePromise = null;
			}
			log.warn("session-manager", "post_admission_maintenance_failed", {
				error: String(error),
			});
		});
		this.postAdmissionMaintenancePromise = starting;
		return starting;
	}

	setAgentStatusHooks(hooks: {
		terminalChanged: (channelId: number) => void;
		sessionClosed: (sessionId: string) => void;
	}): void {
		this.onTerminalChanged = hooks.terminalChanged;
		this.onSessionClosed = hooks.sessionClosed;
	}

	setOnKeeperDegraded(fn: () => void): void {
		this.onKeeperDegraded = fn;
	}

	constructor(opts: {
		workerFp: WorkerFp;
		sink: SessionEventSink;
		createTerminalCore?: (
			cols: number,
			rows: number,
		) => Promise<TerminalCore>;
		sendBinaryUpstream?: (
			channelId: number,
			direction: number,
			endSeq: number,
			bytes: Uint8Array,
		) => TransportSendResult | void;
		sendTerminalMetadataUpstream?: (
			metadata: TerminalMetadataFrame,
		) => TransportSendResult | void;
		terminalCoreCapacity?: TerminalCoreCapacity;
	}) {
		this.workerFp = opts.workerFp;
		this.sink = opts.sink;
		this.terminalCoreCapacity =
			opts.terminalCoreCapacity ?? createWorkerTerminalCoreCapacity();
		this.#createTerminalCore = opts.createTerminalCore ?? _createWtermCore;
		this.sendBinaryUpstream = opts.sendBinaryUpstream ?? null;
		this.sendTerminalMetadataUpstream = opts.sendTerminalMetadataUpstream ?? null;
	}

	reserveTerminalCore(allocationKind: TerminalCoreAllocationKind): TerminalCoreLease {
		switch (allocationKind) {
			case "fresh":
				return this.terminalCoreCapacity.reserveFresh();
			case "adoption":
				return this.terminalCoreCapacity.reserveAdoption();
			case "replacement":
				return this.terminalCoreCapacity.reserveReplacement();
		}
		throw new Error("unsupported terminal core allocation kind");
	}

	async createTerminalCoreForLease(
		lease: TerminalCoreLease,
		cols: number,
		rows: number,
	): Promise<TerminalCore> {
		try {
			return await this.#createTerminalCore(cols, rows);
		} catch (error) {
			lease.release();
			throw error;
		}
	}

	async allocateTerminalCore(
		allocationKind: TerminalCoreAllocationKind,
		cols: number,
		rows: number,
	): Promise<AllocatedTerminalCore> {
		const lease = this.reserveTerminalCore(allocationKind);
		const core = await this.createTerminalCoreForLease(lease, cols, rows);
		return { core, lease };
	}
}
