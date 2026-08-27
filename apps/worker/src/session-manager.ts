// SessionManager owns terminal lifecycle state keyed by ChannelId. It handles
// shell spawn, terminal I/O, and session teardown; transitions emit
// SessionEvents to the coordinator.
import * as scrollback from "./session-scrollback.ts";
import * as emit from "./session-emit.ts";
import * as gitPorts from "./session-git-ports.ts";
import * as spawnFns from "./session-spawn.ts";
import * as resumeFns from "./session-resume.ts";
import * as lifecycle from "./session-lifecycle.ts";
import * as terminalControl from "./session-terminal-control.ts";
import { retireSnapshotCursor } from "./session-snapshot-cursor.ts";
import type { TerminalControlLane, KeeperAdmissionLane } from "./session-control-lanes.ts";
import type { TerminalStreamState } from "./session-terminal-state.ts";
import type { CellGateSuppression } from "./session-emit.ts";
import type { SyncOutputHold } from "./session-sync-output.ts";
import { releaseSyncOutputHold } from "./session-sync-output.ts";
import { diagSnapshot } from "./session-diag-snapshot.ts";
import { _enqueueRawMetadata } from "./session-raw-metadata.ts";
import type { TerminalRequestBudget, TerminalCellSendResult, TransportSendResult } from "./transport/coord-link-types.ts";
import { getMultiplexedPool, type MuxChannelCallbacks } from "./keeper/multiplexed-client.ts";
import { log } from "@roost/shared/log";
import { asChannelId } from "@roost/shared/wire";
import type { PbCellGridChunk, PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import type { SessionEventSink } from "./event-sink.ts";
import type { ChannelState, FsmEvent } from "./fsm.ts";
import type { SessionId, ChannelId, WorkerFp, SessionEvent } from "@roost/shared/wire";
import { STRAY_REAP_INTERVAL_MS } from "./session-constants.ts";
import type { SessionRecord, SessionShellRecord } from "./session-record.ts";
import type { ShellSpec } from "./shell-spec.ts";
export type { SessionRecord, SessionShellRecord } from "./session-record.ts";
interface PendingRawMetadataFrame {
	endSeq: number;
	bytes: Uint8Array;
}

interface PendingRawMetadataQueue {
	frames: PendingRawMetadataFrame[];
	bytes: number;
}


export class SessionManager {
	sessions = new Map<number, SessionRecord>();
	_nextChannel = 1;
	// Caller-minted UUID reservation closes the await-before-register window in
	// session-spawn (wterm-core creation is async). Coordinator idempotency means
	// one command should arrive, but the worker remains the final collision gate.
	private readonly pendingSpawnSessionIds = new Set<SessionId>();
	readonly workerFp: WorkerFp;
	readonly sink: SessionEventSink;
	/** One generation-addressed delivery stream per live channel. The coordinator
	 * owns membership/SCD; the worker only applies this already-aggregated state. */
	terminalStreams = new Map<number, TerminalStreamState>();
	private terminalStreamVersion = 0;
	nextTerminalStreamVersion(): number {
		return ++this.terminalStreamVersion;
	}
	// Last geometry proven by spawn/adoption or an acknowledged keeper resize.
	lastAppliedSize = new Map<number, { cols: number; rows: number }>();
	// Per-channel coalesce timer for cell deltas. A burst of terminal output
	// marks the channel dirty; one delta ships the latest grid per
	// CELL_EMIT_COALESCE_MS.
	cellEmitTimers = new Map<number, ReturnType<typeof setTimeout> | null>();
	cellDirty = new Set<number>();
	// Raw PTY bytes are coordinator-only metadata input. Stage them separately
	// from cells so ready cell frames lead, while retaining FIFO byte order.
	rawMetadataQueues = new Map<number, PendingRawMetadataQueue>();
	rawMetadataTimers = new Map<number, NodeJS.Timeout | null>();
	rawMetadataQueuedBytes = 0;
	// One-shot latency hint: the first PTY chunk after an admitted input may
	// bypass an already-armed trailing cell timer.
	inputSensitiveChannels = new Set<number>();
	// A dropped delta is never guessed forward. The next writable edge emits
	// one authoritative full repair for that channel.
	pendingCellRepairs = new Set<number>();
	// An authoritative full snapshot requested while DEC synchronized output is
	// open must land at the application's paint boundary, not split its atomic
	// repaint. Separate from transport-drop repair: the latter deliberately
	// suppresses output until the worker link reports writable.
	pendingSyncCellSnapshots = new Set<number>();
	strayReaperTimer: ReturnType<typeof setInterval> | null = null;
	// channelId -> consecutive sweeps seen as a stray (see STRAY_REAP_STRIKES).
	strayStrikes = new Map<number, number>();
	// Full terminal-control transactions are serialized per channel. Live resize
	// mutates the existing core at the keeper result-frame boundary; no ordinary
	// control operation allocates a replacement core.
	// Mutual exclusion for whole terminal-control transactions; see
	// session-control-lanes.ts for why this is separate from the write lane.
	terminalControlChains = new Map<number, TerminalControlLane>();
	// Receive-order lane for keeper WRITES (resize request, status query, input,
	// query reply). Held only across a write, never across an ACK.
	keeperAdmissionLane = new Map<number, KeeperAdmissionLane>();
	// Highest resize sequence this worker has WRITTEN for the channel (reserved at
	// write time, not at ACK time).
	channelResizeSeq = new Map<number, number>();
	cellEmissionGates = new Set<number>();
	// has suppressed — so a stalled emitter is attributable from the snapshot.
	cellGateSuppression = new Map<number, CellGateSuppression>();
	// Open DEC 2026 synchronized-output frames, one per channel. Bounded by an
	// armed wall ceiling and a pending-row ceiling so an application that opens a
	// synchronized frame and never closes it cannot withhold cell frames forever.
	syncOutputHolds = new Map<number, SyncOutputHold>();
	// Channels whose core reported a SATURATED OSC 8 link table on the last cell
	// emit. Edge state only, so the Tier-1 signal fires once per flip instead of
	// once per frame; a core rebuild empties the table and clears the flag.
	hyperlinkSaturated = new Set<number>();
	// Structured raw-metadata sink. Passing fields directly avoids constructing
	// and immediately reparsing the old private 11-byte header.
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

	// Sliding-window timestamps of emit_no_session events → keeper.degraded.
	_noSessionBurst: number[] = [];

	// channelId → closedAtMs. A post-close tail emit within RECENTLY_CLOSED_TTL_MS
	// is benign and must NOT count toward _noSessionBurst (else it re-trips
	// keeper.degraded after every reconcile → restart loop).
	recentlyClosed = new Map<number, number>();
	// Sliding-window timestamps of dead-births (spawn → instant zero-byte exit)
	// → keeper.degraded via the same onKeeperDegraded self-heal.
	_deadBirthBurst: number[] = [];
	// Fired when keeper degradation is detected (sustained emit_no_session). main.ts
	// wires this to a grace-gated keeper restart so a degraded survivor self-heals
	// instead of birthing dead PTYs until a manual restart ("can't input").
	onKeeperDegraded: (() => void) | null = null;
	onTerminalChanged: ((channelId: number) => void) | null = null;
	onSessionClosed: ((sessionId: string) => void) | null = null;

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
		// Reverse-reap sweep (see STRAY_REAP_INTERVAL_MS): kill keeper PTYs the
		// worker no longer tracks so a deleted session's process can't outlive it.
		this.strayReaperTimer = setInterval(
			() => void this.reapStrayKeeperChannels(),
			STRAY_REAP_INTERVAL_MS,
		);
		// Eager-init the multiplexed keeper pool so the UDS connect is paid
		// at boot rather than on first spawn. Legacy per-session keepers
		// were removed 2026-06-15 — mux is the only path.
		getMultiplexedPool()
			.ensure()
			.catch((err) =>
				log.warn("session-manager", "mux_keeper_init_failed", {
					error: String(err),
				}),
			);
	}


	nextChannelId(): ChannelId {
		return asChannelId(this._nextChannel++);
	}

	emitEvent(event: SessionEvent): void {
		this.sink.emit(event);
	}

	hasChannel(channelId: number): boolean {
		return this.sessions.has(channelId);
	}

	getByChannel(channelId: number): SessionRecord | undefined {
		return this.sessions.get(channelId);
	}

	getBySessionId(sessionId: string): SessionRecord | undefined {
		for (const r of this.sessions.values()) {
			if (r.sessionId === sessionId) return r;
		}
		return undefined;
	}

	/** Route a legacy PTY input chunk to the multiplexed keeper. Sync v2's
	 * acknowledged input lane calls markInputSensitive at the same admission
	 * point before its keeper write. */
	async input(channelId: number, bytes: Uint8Array): Promise<void> {
		if (!this.sessions.has(channelId)) return;
		this.markInputSensitive(channelId);
		getMultiplexedPool().input(channelId, bytes);
	}

	markInputSensitive(channelId: number): void {
		if (this.sessions.has(channelId)) this.inputSensitiveChannels.add(channelId);
	}
	writeTerminalInput(
		sessionId: string,
		inputSeq: bigint,
		bytes: Uint8Array,
		budget?: TerminalRequestBudget,
	): Promise<terminalControl.WorkerInputResult> {
		return terminalControl.writeTerminalInput.call(this, sessionId, inputSeq, bytes, budget);
	}

	applyTerminalStreamState(
		intent: terminalControl.WorkerTerminalStreamIntent,
	): Promise<terminalControl.WorkerTerminalStreamResult> {
		return terminalControl.applyTerminalStreamState.call(this, intent);
	}

	requestTerminalSnapshot(sessionId: string, streamId: string): void {
		terminalControl.requestTerminalSnapshot.call(this, sessionId, streamId);
	}
	invalidateTerminalStreamsForReconnect(): void {
		for (const [channelId, current] of this.terminalStreams) {
			retireSnapshotCursor(this, channelId, current);
			const baseline = Promise.withResolvers<boolean>();
			baseline.resolve(true);
			this.terminalStreams.set(channelId, {
				streamId: current.streamId,
				enabled: false,
				cols: 0,
				rows: 0,
				version: this.nextTerminalStreamVersion(),
				baselineReady: true,
				coreValid: current.coreValid,
				baselineDirty: false,
				snapshotCursor: null,
				resizeCapture: current.resizeCapture,
				baselineInstalled: baseline.promise,
				baselinePromisePending: false,
				resolveBaselineInstalled: baseline.resolve,
			});
		}
	}


	/** Return all live sessions for snapshot emission. */
	allSessions() {
		return Array.from(this.sessions.values());
	}

	/** Install a fresh immutable full-snapshot cursor for the current stream. */
	installTerminalBaseline(channelId: ChannelId): void {
		emit.installTerminalBaseline.call(this, channelId);
	}

	appendScrollback(channelId: number, chunk: Buffer): number {
		return scrollback.appendScrollback.call(this, channelId, chunk);
	}

	/** Capture-lane ingest: retain + scan, never the frozen core. */
	appendCapturedScrollback(channelId: number, chunk: Buffer): number {
		return scrollback.appendCapturedScrollback.call(this, channelId, chunk);
	}

	emitUpstreamChunk(channelId: number, chunk: Buffer): void {
		return emit.emitUpstreamChunk.call(this, channelId, chunk);
	}

	_hasEnabledStream(channelId: number): boolean {
		return emit._hasEnabledStream.call(this, channelId);
	}


	_scheduleCellEmit(channelId: number, promoteInputEcho = false): void {
		return emit._scheduleCellEmit.call(this, channelId, promoteInputEcho);
	}
	_enqueueRawMetadata(channelId: number, endSeq: number, chunk: Buffer): void {
		return _enqueueRawMetadata.call(this, channelId, endSeq, chunk);
	}

	resumeTerminalSnapshots(): void {
		return emit.resumeTerminalSnapshots.call(this);
	}

	_disposeOutputState(channelId: number): void {
		return emit._disposeOutputState.call(this, channelId);
	}

	/** Retire an open synchronized-output hold. The resize transaction reaches
	 *  it through here rather than through an import cycle. */
	_releaseSyncOutputHold(channelId: number): void {
		return releaseSyncOutputHold(this, channelId);
	}

	emitCellFrame(channelId: number, force: boolean): void {
		return emit.emitCellFrame.call(this, channelId, force);
	}

	muxCallbacks(channelId: number): MuxChannelCallbacks {
		return emit.muxCallbacks.call(this, channelId);
	}


	/** The shell record for a channel, or undefined when the channel is gone. */
	shellByChannel(channelId: number): SessionShellRecord | undefined {
		return this.sessions.get(channelId);
	}

	_startGitBranch(rec: SessionRecord): void {
		return gitPorts._startGitBranch.call(this, rec);
	}

	_startPorts(rec: SessionRecord): void {
		return gitPorts._startPorts.call(this, rec);
	}

	_resolvePorts(rec: SessionRecord): Promise<void> {
		return gitPorts._resolvePorts.call(this, rec);
	}

	_resolvePr(rec: SessionRecord): Promise<void> {
		return gitPorts._resolvePr.call(this, rec);
	}


	spawnShell(
		cwd: string,
		cols?: number,
		rows?: number,
		targetSessionId?: SessionId,
	): Promise<SessionRecord> {
		if (
			targetSessionId
			&& (
				this.getBySessionId(targetSessionId)
				|| this.pendingSpawnSessionIds.has(targetSessionId)
			)
		) {
			return Promise.reject(new Error(`session ${targetSessionId} is already live or spawning`));
		}
		if (targetSessionId) this.pendingSpawnSessionIds.add(targetSessionId);
		const spawned = spawnFns.spawnShell.call(
			this,
			cwd,
			cols,
			rows,
			targetSessionId,
		);
		return targetSessionId
			? spawned.finally(() => this.pendingSpawnSessionIds.delete(targetSessionId))
			: spawned;
	}




	respawnIfMissing(sessionId: SessionId, cwd: string, cols: number, rows: number): Promise<SessionRecord> {
		return spawnFns.respawnIfMissing.call(this, sessionId, cwd, cols, rows);
	}

	resume(opts: { sessionId: SessionId; channelId: ChannelId; kind: "shell"; cwd: string; shellSpec: ShellSpec }): Promise<boolean> {
		return resumeFns.resume.call(this, opts);
	}

	respawn(opts: { oldSessionId: SessionId; cwd: string; kind: "shell"; cols?: number; rows?: number; shellSpec?: ShellSpec }): Promise<void> {
		return resumeFns.respawn.call(this, opts);
	}

	advanceChannelCounterPastKeeper(): Promise<void> {
		return lifecycle.advanceChannelCounterPastKeeper.call(this);
	}

	reapStrayKeeperChannels(): Promise<number> {
		return lifecycle.reapStrayKeeperChannels.call(this);
	}

	emitClosedTombstone(sessionId: SessionId): void {
		return lifecycle.emitClosedTombstone.call(this, sessionId);
	}

	kill(channelId: number): void {
		return lifecycle.kill.call(this, channelId);
	}

	closedByKeeper(channelId: number, exitCode: number | null): void {
		return lifecycle.closedByKeeper.call(this, channelId, exitCode);
	}

	_checkDeadBirth(rec: SessionRecord): void {
		return lifecycle._checkDeadBirth.call(this, rec);
	}

	_dropChannelState(channelId: number): void {
		return lifecycle._dropChannelState.call(this, channelId);
	}

	markRecentlyClosed(channelId: number): void {
		return lifecycle.markRecentlyClosed.call(this, channelId);
	}

	diagSnapshot(): Record<string, unknown> {
		return diagSnapshot.call(this);
	}

	_onTransition(sessionId: SessionId, channelId: ChannelId, from: ChannelState, to: ChannelState, event: FsmEvent): void {
		return lifecycle._onTransition.call(this, sessionId, channelId, from, to, event);
	}



	dispose(): void {
		return lifecycle.dispose.call(this);
	}

}
