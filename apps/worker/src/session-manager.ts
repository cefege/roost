// SessionManager owns terminal lifecycle state keyed by ChannelId. It handles
// shell spawn, terminal I/O, and session teardown; transitions emit
// SessionEvents to the coordinator.

import * as scrollback from "./session-scrollback.ts";
import * as emit from "./session-emit.ts";
import * as gitPorts from "./session-git-ports.ts";
import * as viewport from "./session-viewport.ts";
import * as spawnFns from "./session-spawn.ts";
import * as agentSpawnFns from "./agent/spawn-agent.ts";
import * as resumeFns from "./session-resume.ts";
import * as lifecycle from "./session-lifecycle.ts";

import { getMultiplexedPool, type MuxChannelCallbacks } from "./keeper/multiplexed-client.ts";
import { log, asChannelId } from "@roost/shared";
import type { TerminalCore } from "@wterm/core";
import type { AgentEntriesFrame as PbAgentEntriesFrame } from "@roost/shared/proto/sync_pb";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import type { SessionEventSink } from "./event-sink.ts";
import type { ChannelState, FsmEvent } from "./fsm.ts";
import type {
	SessionId,
	ChannelId,
	WorkerFp,
	SessionEvent,
} from "@roost/shared";
import {
	VIEWPORT_REAPER_INTERVAL_MS,
	STRAY_REAP_INTERVAL_MS,
} from "./session-constants.ts";
import type { SessionRecord, SessionShellRecord, ViewportClaim } from "./session-record.ts";

export type { SessionRecord, SessionShellRecord, SessionAgentRecord } from "./session-record.ts";

export class SessionManager {
	sessions = new Map<number, SessionRecord>();
	_nextChannel = 1;
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
	viewportReaperTimer: ReturnType<typeof setInterval> | null = null;
	strayReaperTimer: ReturnType<typeof setInterval> | null = null;
	// channelId -> consecutive sweeps seen as a stray (see STRAY_REAP_STRIKES).
	strayStrikes = new Map<number, number>();
	// OPT2-1: per-channel serializer for deterministic wtermCore rebuilds on
	// resize. @wterm/core's in-place resize is asymmetric (shrink pushes rows
	// to scrollback, grow appends blanks, never reverses) and PATH-DEPENDENT:
	// the same final cols×rows yields a different grid depending on the resize
	// history → the "phone rotation sometimes mangles" coin-flip. We instead
	// rebuild a fresh core at the target size and replay the raw ring, which
	// is the single source of truth: same ring + same size = same grid, every
	// time. Chained so rapid resizes don't overlap; the last replay wins.
	_wtermRebuildChain = new Map<number, Promise<void>>();
	// Bytes upstream sink. The mux pool callback path (muxCallbacks
	// below) routes every PTY chunk through emitUpstreamChunk → this
	// sink → CoordWorkerUp.binary frame.
	readonly sendBinaryUpstream: ((bytes: Uint8Array) => void) | null;
	// R11 cell-grid cell-shipping sink. Cells are the ONLY renderer since cell-phase-4
	// (byte renderer deleted, commit e8f450b9) — the SPA paints nothing but
	// these frames. null only in tests that don't wire a cell sink; the
	// _hasActiveViewer gate already suppresses emission to unwatched channels.
	readonly sendCellGridUpstream:
		| ((channelId: number, frame: PbCellGridFrame) => void)
		| null;
	// Agent transcript sink (kind === "agent" sessions only). null in tests that
	// wire no transport; the controller then just fills its ring and the
	// SessionsGetAgentEntries backfill still serves it.
	readonly sendAgentEntriesUpstream:
		| ((frame: PbAgentEntriesFrame) => void)
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

	setOnKeeperDegraded(fn: () => void): void {
		this.onKeeperDegraded = fn;
	}

	constructor(opts: {
		workerFp: WorkerFp;
		sink: SessionEventSink;
		sendBinaryUpstream?: (bytes: Uint8Array) => void;
		sendCellGridUpstream?: (channelId: number, frame: PbCellGridFrame) => void;
		sendAgentEntriesUpstream?: (frame: PbAgentEntriesFrame) => void;
	}) {
		this.workerFp = opts.workerFp;
		this.sink = opts.sink;
		this.sendBinaryUpstream = opts.sendBinaryUpstream ?? null;
		this.sendCellGridUpstream = opts.sendCellGridUpstream ?? null;
		this.sendAgentEntriesUpstream = opts.sendAgentEntriesUpstream ?? null;
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

	/** Route a PTY input chunk to the multiplexed keeper. */
	async input(channelId: number, bytes: Uint8Array): Promise<void> {
		if (!this.sessions.has(channelId)) return;
		getMultiplexedPool().input(channelId, bytes);
	}

	/** Return all live sessions for snapshot emission. */
	allSessions() {
		return Array.from(this.sessions.values());
	}

	/** R11 — force a full cell frame upstream (a fresh SPA viewer attaching
	 *  needs the whole grid; live deltas follow). */
	emitCellSnapshot(channelId: ChannelId, tailRows?: number): void {
		this.emitCellFrame(channelId, true, tailRows);
	}

	appendScrollback(channelId: number, chunk: Buffer): number {
		return scrollback.appendScrollback.call(this, channelId, chunk);
	}


	_answerTerminalQueries(core: TerminalCore, channelId: number, chunk: Uint8Array): void {
		return scrollback._answerTerminalQueries.call(this, core, channelId, chunk);
	}

	emitUpstreamChunk(channelId: number, chunk: Buffer): void {
		return emit.emitUpstreamChunk.call(this, channelId, chunk);
	}

	_hasActiveViewer(channelId: number): boolean {
		return emit._hasActiveViewer.call(this, channelId);
	}


	_scheduleCellEmit(channelId: number): void {
		return emit._scheduleCellEmit.call(this, channelId);
	}

	emitCellFrame(channelId: number, force: boolean, tailRows?: number): void {
		return emit.emitCellFrame.call(this, channelId, force, tailRows);
	}

	muxCallbacks(channelId: number): MuxChannelCallbacks {
		return emit.muxCallbacks.call(this, channelId);
	}


	/** The SHELL record for a channel, or undefined when the channel is gone or
	 *  belongs to an agent session (which has no terminal state at all). Every
	 *  PTY-only path narrows through this rather than re-testing `kind`. */
	shellByChannel(channelId: number): SessionShellRecord | undefined {
		const rec = this.sessions.get(channelId);
		return rec?.kind === "shell" ? rec : undefined;
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

	claimViewport(channelId: number, viewerFp: string, cols: number, rows: number, clientSeq?: number, cause?: number, heldSbTotal?: number): void {
		return viewport.claimViewport.call(this, channelId, viewerFp, cols, rows, clientSeq, cause, heldSbTotal);
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

	_recomputeViewport(channelId: number, heldSbTotal?: number): void {
		return viewport._recomputeViewport.call(this, channelId, heldSbTotal);
	}

	_scheduleWtermRebuild(channelId: number, cols: number, rows: number, heldSbTotal?: number): void {
		return viewport._scheduleWtermRebuild.call(this, channelId, cols, rows, heldSbTotal);
	}

	_rebuildWtermCore(channelId: number, cols: number, rows: number, heldSbTotal?: number): Promise<void> {
		return viewport._rebuildWtermCore.call(this, channelId, cols, rows, heldSbTotal);
	}

	spawnShell(cwd: string, cols?: number, rows?: number, targetSessionId?: SessionId): Promise<SessionRecord> {
		return spawnFns.spawnShell.call(this, cwd, cols, rows, targetSessionId);
	}

	spawnAgent(cwd: string, opts?: agentSpawnFns.SpawnAgentOptions): Promise<SessionRecord> {
		return agentSpawnFns.spawnAgent.call(this, cwd, opts);
	}



	respawnIfMissing(sessionId: SessionId, cwd: string, cols: number, rows: number): Promise<SessionRecord> {
		return spawnFns.respawnIfMissing.call(this, sessionId, cwd, cols, rows);
	}

	resume(opts: { sessionId: SessionId; channelId: ChannelId; kind: "shell"; cwd: string }): Promise<boolean> {
		return resumeFns.resume.call(this, opts);
	}

	respawn(opts: { oldSessionId: SessionId; cwd: string; kind: "shell"; cols?: number; rows?: number }): Promise<void> {
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
