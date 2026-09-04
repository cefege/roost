// SessionManager owns terminal lifecycle state keyed by ChannelId. It handles
// shell spawn, terminal I/O, and session teardown; transitions emit
// SessionEvents to the coordinator.
import * as scrollback from "./session-scrollback.ts";
import * as emit from "./session-emit.ts";
import * as gitPorts from "./session-git-ports.ts";
import * as spawnFns from "./session-spawn.ts";
import * as resumeFns from "./session-resume.ts";
import * as respawnFns from "./session-respawn.ts";
import * as lifecycle from "./session-lifecycle.ts";
import * as terminalControl from "./session-terminal-control.ts";
import { retireSnapshotCursor } from "./session-snapshot-cursor.ts";
import { SessionManagerState } from "./session-manager-state.ts";
import { releaseSyncOutputHold } from "./session-sync-output.ts";
import { diagSnapshot } from "./session-diag-snapshot.ts";
import { _enqueueRawMetadata } from "./session-raw-metadata.ts";
import type { TerminalRequestBudget } from "./transport/coord-link-types.ts";
import { WORKER_SNAPSHOT_MAX_SESSIONS } from "./transport/coord-link-constants.ts";
import { getMultiplexedPool, type MuxChannelCallbacks } from "./keeper/multiplexed-client.ts";
import { asChannelId } from "@roost/shared/wire";
import {
	isFatalSessionEventError,
	SessionLifecycleOutboxFullError,
	type DurableLifecycleKind,
	type LifecycleReservation,
} from "./event-sink.ts";
import type { ChannelState, FsmEvent } from "./fsm.ts";
import type { SessionId, ChannelId, SessionEvent } from "@roost/shared/wire";
import type { SessionRecord, SessionShellRecord } from "./session-record.ts";
import type { ShellSpec } from "./shell-spec.ts";
export type { SessionRecord, SessionShellRecord } from "./session-record.ts";
export function isLifecycleOutboxFullError(
	error: unknown,
): error is SessionLifecycleOutboxFullError {
	return error instanceof SessionLifecycleOutboxFullError;
}

export function isSessionLifecycleDurabilityError(error: unknown): boolean {
	return isFatalSessionEventError(error);
}



export class SessionManager extends SessionManagerState {
	nextTerminalStreamVersion(): number {
		return ++this.terminalStreamVersion;
	}


	nextChannelId(): ChannelId {
		return asChannelId(this._nextChannel++);
	}

	reserveLifecycleEvent(kind: DurableLifecycleKind): LifecycleReservation {
		return this.sink.reserveLifecycleEvent(kind);
	}

	holdLifecycleEvent(reservation: LifecycleReservation): void {
		this.sink.holdLifecycleEvent(reservation);
	}

	releaseLifecycleEvent(reservation: LifecycleReservation): void {
		this.sink.releaseLifecycleEvent(reservation);
	}

	emitEvent(event: SessionEvent, reservation?: LifecycleReservation): void {
		this.sink.emit(event, reservation);
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
		existingLogicalSession = false,
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
		if (
			!existingLogicalSession
			&& this.sessions.size + this.pendingSnapshotSessionAdmissions >= WORKER_SNAPSHOT_MAX_SESSIONS
		) {
			return Promise.reject(new Error("worker session snapshot limit reached"));
		}
		const countedAdmission = !existingLogicalSession;
		if (countedAdmission) this.pendingSnapshotSessionAdmissions += 1;
		let openedReservation: LifecycleReservation;
		try {
			openedReservation = this.reserveLifecycleEvent("opened");
		} catch (error) {
			if (countedAdmission) this.pendingSnapshotSessionAdmissions -= 1;
			throw error;
		}
		let closeReservation: LifecycleReservation;
		try {
			closeReservation = this.reserveLifecycleEvent("closed");
		} catch (error) {
			this.releaseLifecycleEvent(openedReservation);
			if (countedAdmission) this.pendingSnapshotSessionAdmissions -= 1;
			throw error;
		}
		if (targetSessionId) this.pendingSpawnSessionIds.add(targetSessionId);
		return spawnFns.spawnShell.call(
			this,
			cwd,
			cols,
			rows,
			targetSessionId,
			openedReservation,
			closeReservation,
		).finally(() => {
			if (targetSessionId) this.pendingSpawnSessionIds.delete(targetSessionId);
			if (countedAdmission) this.pendingSnapshotSessionAdmissions -= 1;
		});
	}




	async respawnIfMissing(
		sessionId: SessionId,
		cwd: string,
		cols: number,
		rows: number,
	): Promise<SessionRecord> {
		const existing = this.getBySessionId(sessionId);
		if (existing) return existing;
		if (this.pendingSpawnSessionIds.has(sessionId)) {
			throw new Error(`session ${sessionId} is already live or spawning`);
		}
		this.pendingSpawnSessionIds.add(sessionId);
		try {
			await this.respawn({
				oldSessionId: sessionId,
				cwd,
				kind: "shell",
				cols,
				rows,
			});
			const respawned = this.getBySessionId(sessionId);
			if (!respawned) {
				throw new Error(`respawned session ${sessionId} is not live`);
			}
			return respawned;
		} finally {
			this.pendingSpawnSessionIds.delete(sessionId);
		}
	}

	resume(
		opts: { sessionId: SessionId; channelId: ChannelId; kind: "shell"; cwd: string; shellSpec: ShellSpec },
		closeReservation?: LifecycleReservation,
	): Promise<boolean> {
		return resumeFns.resume.call(
			this,
			opts,
			closeReservation ?? this.reserveLifecycleEvent("closed"),
		);
	}

	respawn(
		opts: { oldSessionId: SessionId; cwd: string; kind: "shell"; cols?: number; rows?: number; shellSpec?: ShellSpec },
		reservations?: {
			event: LifecycleReservation;
			close: LifecycleReservation;
		},
	): Promise<void> {
		if (reservations) {
			return respawnFns.respawn.call(
				this,
				opts,
				reservations.event,
				reservations.close,
				false,
			);
		}
		const eventReservation = this.reserveLifecycleEvent("respawned");
		let closeReservation: LifecycleReservation;
		try {
			closeReservation = this.reserveLifecycleEvent("closed");
		} catch (error) {
			this.releaseLifecycleEvent(eventReservation);
			throw error;
		}
		return respawnFns.respawn.call(
			this,
			opts,
			eventReservation,
			closeReservation,
			true,
		);
	}

	advanceChannelCounterPastKeeper(): Promise<void> {
		return lifecycle.advanceChannelCounterPastKeeper.call(this);
	}

	reapStrayKeeperChannels(): Promise<number> {
		return lifecycle.reapStrayKeeperChannels.call(this);
	}

	emitClosedTombstone(
		sessionId: SessionId,
		reservation?: LifecycleReservation,
	): void {
		return lifecycle.emitClosedTombstone.call(this, sessionId, reservation);
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
