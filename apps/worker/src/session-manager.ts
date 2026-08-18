// SessionManager owns terminal lifecycle state keyed by ChannelId. It handles
// shell spawn, terminal I/O, and session teardown; transitions emit
// SessionEvents to the coordinator.

import * as scrollback from "./session-scrollback.ts";
import * as emit from "./session-emit.ts";
import * as gitPorts from "./session-git-ports.ts";
import * as viewport from "./session-viewport.ts";
import * as spawnFns from "./session-spawn.ts";
import * as resumeFns from "./session-resume.ts";
import type { InitialViewportPreclaim } from "./session-spawn.ts";
import * as lifecycle from "./session-lifecycle.ts";
import * as terminalControl from "./session-terminal-control.ts";
import type { TerminalControlLane, KeeperAdmissionLane } from "./session-control-lanes.ts";
import type { ResizeCapture } from "./session-resize-capture.ts";
import { rebuildTerminalCore } from "./session-resize-capture.ts";
import type { CellGateSuppression, SyncOutputHold } from "./session-emit.ts";
import type { TerminalRequestBudget } from "./transport/coord-link-types.ts";

import { getMultiplexedPool, type MuxChannelCallbacks } from "./keeper/multiplexed-client.ts";
import { log } from "@roost/shared/log";
import { asChannelId } from "@roost/shared/wire";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import type { TransportSendResult } from "./transport/coord-link-types.ts";
import type { SessionEventSink } from "./event-sink.ts";
import type { ChannelState, FsmEvent } from "./fsm.ts";
import type { SessionId, ChannelId, WorkerFp, SessionEvent } from "@roost/shared/wire";
import {
	VIEWPORT_REAPER_INTERVAL_MS,
	STRAY_REAP_INTERVAL_MS,
} from "./session-constants.ts";
import type { SessionRecord, SessionShellRecord, ViewportClaim } from "./session-record.ts";
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
	// Per-channel viewport claims keyed by viewer fingerprint. Empty map
	// entry kept until the channel is killed (cheap, ~24 bytes). PTY size
	// recomputed on every claim/withdraw/reap; SIGWINCH only fires if the
	// resulting SCD changes from the last applied size.
	viewportClaims = new Map<number, Map<string, ViewportClaim>>();
	// Cache of the last (cols,rows) we actually applied to the PTY, keyed
	// by channelId. Skip SIGWINCH if the recompute lands on the same value.
	lastAppliedSize = new Map<number, { cols: number; rows: number }>();
	// Deferred-removal timers for withdraw hysteresis, keyed
	// `${channelId}:${viewerFp}`. A re-claim clears the pending timer so a
	// refresh doesn't flap the SCD size. See VIEWPORT_WITHDRAW_GRACE_MS.
	pendingWithdraws = new Map<string, ReturnType<typeof setTimeout>>();
	// Per-channel coalesce timer for cell deltas. A burst of terminal output
	// marks the channel dirty; one delta ships the latest grid per
	// CELL_EMIT_COALESCE_MS.
	cellEmitTimers = new Map<number, ReturnType<typeof setTimeout>>();
	cellDirty = new Set<number>();
	// Raw PTY bytes are coordinator-only metadata input. Stage them separately
	// from cells so ready cell frames lead, while retaining FIFO byte order.
	rawMetadataQueues = new Map<number, PendingRawMetadataQueue>();
	rawMetadataTimers = new Map<number, NodeJS.Timeout>();
	rawMetadataQueuedBytes = 0;
	// One-shot latency hint: the first PTY chunk after an admitted input may
	// bypass an already-armed trailing cell timer.
	inputSensitiveChannels = new Set<number>();
	// A dropped delta is never guessed forward. The next writable edge emits
	// one authoritative full repair for that channel.
	pendingCellRepairs = new Set<number>();
	viewportReaperTimer: ReturnType<typeof setInterval> | null = null;
	strayReaperTimer: ReturnType<typeof setInterval> | null = null;
	// channelId -> consecutive sweeps seen as a stray (see STRAY_REAP_STRIKES).
	strayStrikes = new Map<number, number>();
	// OPT2-1: rebuilding a fresh core and replaying the raw ring is the ONLY
	// resize path. @wterm/core's in-place resize is asymmetric (shrink pushes rows
	// to scrollback, grow appends blanks, never reverses) and PATH-DEPENDENT: the
	// same final cols×rows yields a different grid depending on the resize history
	// → the "phone rotation sometimes mangles" coin-flip. Same ring + same size =
	// same grid, every time. The rebuild now runs INSIDE the owning terminal
	// control transaction (see terminalControlChains) instead of on its own
	// uncorrelated chain, so one decision can never build two cores.
	// Per-channel count of actual core swaps. Bounded (one number per live
	// channel) and monotonic — the once-only rebuild is observable, not assumed.
	coreRebuilds = new Map<number, number>();
	// Mutual exclusion for whole terminal-control transactions; see
	// session-control-lanes.ts for why this is separate from the write lane.
	terminalControlChains = new Map<number, TerminalControlLane>();
	// Receive-order lane for keeper WRITES (resize request, status query, input,
	// query reply). Held only across a write, never across an ACK.
	keeperAdmissionLane = new Map<number, KeeperAdmissionLane>();
	// Highest resize sequence this worker has WRITTEN for the channel (reserved at
	// write time, not at ACK time).
	channelResizeSeq = new Map<number, number>();
	// Channels whose sequence floor is not trustworthy: an ambiguous resize means
	// the keeper may have consumed a sequence we cannot prove. The next allocation
	// recovers authoritative keeper state first.
	resizeFloorInvalid = new Set<number>();
	cellEmissionGates = new Set<number>();
	// Installed atomically with the gate for a geometry-uncertain transaction: the
	// canonical core is frozen while captured bytes still reach the raw ring and
	// the coordinator metadata lane. Replaces the unbounded postResizeOutput.
	resizeCaptures = new Map<number, ResizeCapture>();
	// Which gate is withholding cell frames, since when, and how many frames it
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
	// Cells are the sole renderer. A void result is accepted only for narrow
	// legacy test sinks; production CoordLink always returns a truthful result.
	readonly sendCellGridUpstream:
		| ((channelId: number, frame: PbCellGridFrame) => TransportSendResult | void)
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
		) => TransportSendResult | void;
	}) {
		this.workerFp = opts.workerFp;
		this.sink = opts.sink;
		this.sendBinaryUpstream = opts.sendBinaryUpstream ?? null;
		this.sendCellGridUpstream = opts.sendCellGridUpstream ?? null;
		// Viewport-claim reaper. Every 5s: drop claims older than 60s,
		// recompute SCD per affected channel, SIGWINCH if changed. Catches
		// dead browsers that didn't get to send a withdraw (kill -9, WiFi
		// drop, OS sleep) so they don't pin the PTY small forever.
		this.viewportReaperTimer = setInterval(
			() => this._reapViewportClaims(),
			VIEWPORT_REAPER_INTERVAL_MS,
		);
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

	applyTerminalViewport(intent: terminalControl.WorkerViewportIntent): Promise<terminalControl.WorkerViewportResult> {
		return terminalControl.applyTerminalViewport.call(this, intent);
	}


	/** Return all live sessions for snapshot emission. */
	allSessions() {
		return Array.from(this.sessions.values());
	}

	/** R11 — force a full cell frame upstream (a fresh SPA viewer attaching
	 *  needs the whole grid; live deltas follow). */
	emitCellSnapshot(channelId: ChannelId): void {
		this.emitCellFrame(channelId, true);
	}

	resnapshotClaimedSessions(): void {
		return emit.resnapshotClaimedSessions.call(this);
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

	_hasActiveViewer(channelId: number): boolean {
		return emit._hasActiveViewer.call(this, channelId);
	}


	_scheduleCellEmit(channelId: number, promoteInputEcho = false): void {
		return emit._scheduleCellEmit.call(this, channelId, promoteInputEcho);
	}

	_enqueueRawMetadata(channelId: number, endSeq: number, chunk: Buffer): void {
		return emit._enqueueRawMetadata.call(this, channelId, endSeq, chunk);
	}

	flushPendingCellRepairs(): void {
		return emit.flushPendingCellRepairs.call(this);
	}

	_disposeOutputState(channelId: number): void {
		return emit._disposeOutputState.call(this, channelId);
	}

	/** Retire an open synchronized-output hold. The only importer of
	 *  session-emit's values is this class, so the resize transaction reaches it
	 *  through here rather than through an import cycle. */
	_releaseSyncOutputHold(channelId: number): void {
		return emit.releaseSyncOutputHold(this, channelId);
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

	claimViewport(channelId: number, viewerFp: string, cols: number, rows: number, clientSeq?: number | bigint, cause?: number, heldCellSeq?: number): void {
		return viewport.claimViewport.call(this, channelId, viewerFp, cols, rows, clientSeq, cause, heldCellSeq);
	}

	withdrawViewport(channelId: number, viewerFp: string): void {
		return viewport.withdrawViewport.call(this, channelId, viewerFp);
	}

	_cancelPendingWithdraw(channelId: number, viewerFp: string): void {
		return viewport._cancelPendingWithdraw.call(this, channelId, viewerFp);
	}

	_reapViewportClaims(): void {
		return viewport._reapViewportClaims.call(this);
	}

	/** Single owner of an SCD size decision: claim, deferred withdraw, and the
	 *  freshness reaper all queue here. */
	reconcileTerminalViewport(channelId: number): void {
		return terminalControl.reconcileTerminalViewport.call(this, channelId);
	}

	/** Rebuild the core at an exact size from the retained ring, with no capture
	 *  to split at. Production always rebuilds through a capture (inside the
	 *  owning transaction); this is the determinism reference. */
	_rebuildWtermCore(channelId: number, cols: number, rows: number): Promise<boolean> {
		return rebuildTerminalCore(this, channelId, cols, rows, null);
	}

	spawnShell(
		cwd: string,
		cols?: number,
		rows?: number,
		targetSessionId?: SessionId,
		initialViewport?: InitialViewportPreclaim,
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
			initialViewport,
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

	diagSnapshot(): Record<string, unknown> {
		return lifecycle.diagSnapshot.call(this);
	}

	_onTransition(sessionId: SessionId, channelId: ChannelId, from: ChannelState, to: ChannelState, event: FsmEvent): void {
		return lifecycle._onTransition.call(this, sessionId, channelId, from, to, event);
	}



	dispose(): void {
		return lifecycle.dispose.call(this);
	}

}
