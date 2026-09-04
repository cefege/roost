// SessionManagerState owns the mutable maps and transport callbacks for terminal sessions.
// SessionManager extends it with lifecycle operations while collaborator modules share the state.
// Keeper maintenance starts only after boot admits the complete coordinator session set.

import type { TerminalControlLane, KeeperAdmissionLane } from "./session-control-lanes.ts";
import type { TerminalStreamState } from "./session-terminal-state.ts";
import type { CellGateSuppression } from "./session-emit.ts";
import type { SyncOutputHold } from "./session-sync-output.ts";
import type { TerminalCellSendResult, TransportSendResult } from "./transport/coord-link-types.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import { log } from "@roost/shared/log";
import type { PbCellGridChunk, PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import type { SessionEventSink } from "./event-sink.ts";
import type { SessionId, WorkerFp } from "@roost/shared/wire";
import { STRAY_REAP_INTERVAL_MS } from "./session-constants.ts";
import type { SessionRecord } from "./session-record.ts";

interface PendingRawMetadataFrame {
	endSeq: number;
	bytes: Uint8Array;
}

interface PendingRawMetadataQueue {
	frames: PendingRawMetadataFrame[];
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
	terminalStreams = new Map<number, TerminalStreamState>();
	protected terminalStreamVersion = 0;
	lastAppliedSize = new Map<number, { cols: number; rows: number }>();
	cellEmitTimers = new Map<number, NodeJS.Timeout | null>();
	cellDirty = new Set<number>();
	rawMetadataQueues = new Map<number, PendingRawMetadataQueue>();
	rawMetadataTimers = new Map<number, NodeJS.Timeout | null>();
	rawMetadataQueuedBytes = 0;
	inputSensitiveChannels = new Set<number>();
	pendingCellRepairs = new Set<number>();
	pendingSyncCellSnapshots = new Set<number>();
	strayReaperTimer: NodeJS.Timeout | null = null;
	postAdmissionMaintenancePromise: Promise<void> | null = null;
	strayStrikes = new Map<number, number>();
	terminalControlChains = new Map<number, TerminalControlLane>();
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
	readonly sendCellGridUpstream:
		| ((channelId: number, frame: PbCellGridFrame) => TerminalCellSendResult | void)
		| null;
	readonly sendCellGridChunkUpstream:
		| ((channelId: number, chunk: PbCellGridChunk) => TerminalCellSendResult | void)
		| null;

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
		sendBinaryUpstream?: (
			channelId: number,
			direction: number,
			endSeq: number,
			bytes: Uint8Array,
		) => TransportSendResult | void;
		sendCellGridUpstream?: (
			channelId: number,
			frame: PbCellGridFrame,
		) => TerminalCellSendResult | void;
		sendCellGridChunkUpstream?: (
			channelId: number,
			chunk: PbCellGridChunk,
		) => TerminalCellSendResult | void;
	}) {
		this.workerFp = opts.workerFp;
		this.sink = opts.sink;
		this.sendBinaryUpstream = opts.sendBinaryUpstream ?? null;
		this.sendCellGridUpstream = opts.sendCellGridUpstream ?? null;
		this.sendCellGridChunkUpstream = opts.sendCellGridChunkUpstream ?? null;
	}
}
