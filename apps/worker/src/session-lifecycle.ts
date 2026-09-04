// Keeper reconcile/stray-reap, close/tombstone teardown, dead-birth self-heal,
// FSM transition emit, and per-channel state drop. Split out of
// session-manager.ts (400-line cap); bodies byte-for-byte unchanged, called
// with a SessionManager `this`.

import type { SessionManager } from "./session-manager.ts";
import { retireSnapshotCursor } from "./session-snapshot-cursor.ts";
import type { SessionRecord } from "./session-record.ts";
import type { LifecycleReservation } from "./event-sink.ts";
import type { SessionId, ChannelId } from "@roost/shared/wire";
import { diag, signal } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import type { ChannelState, FsmEvent } from "./fsm.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import * as byteCapture from "./diag/byte-capture.ts";
import {
	RECENTLY_CLOSED_TTL_MS,
	STRAY_REAP_STRIKES,
	KEEPER_DEGRADED_WINDOW_MS,
	KEEPER_DEAD_BIRTH_THRESHOLD,
	DEAD_BIRTH_LIFETIME_MS,
} from "./session-constants.ts";

/** After a worker restart the surviving keeper may still hold channels coord
 *  no longer lists (orphaned PTYs from earlier in its long life). resume()
 *  only advances _nextChannel past COORD-known channels, so a fresh spawn can
 *  pick an id the keeper already has → keeper replies SpawnErr "channel_id in
 *  use" and the new terminal fails. Advance the counter past the keeper's
 *  ACTUAL max channel so a new spawn can never collide. Called at reconcile. */
export async function advanceChannelCounterPastKeeper(this: SessionManager): Promise<void> {
	try {
		const live = await getMultiplexedPool().listChannels();
		const maxKeeperCh = live.reduce((m, c) => Math.max(m, c.channelId), 0);
		if (maxKeeperCh >= this._nextChannel) {
			this._nextChannel = maxKeeperCh + 1;
			log.info("session-manager", "channel_counter_advanced", {
				maxKeeperCh,
				nextChannel: this._nextChannel,
			});
		}
	} catch (e) {
		log.warn("session-manager", "channel_counter_advance_failed", {
			error: String(e),
		});
	}
}

/** Authoritative-set GC. this.sessions is the worker's live intent (kill()
 *  deletes from it AND emits `closed`), so any keeper channel NOT in it is a
 *  ghost: a deleted session whose KillChild no-op'd (channel-mismatched keeper
 *  — multiplexed-main.ts:400), or a leftover from a prior keeper generation.
 *  The optimistic `closedByKeeper` at kill() removes the coord row before the
 *  process is confirmed dead; this is the backstop that makes the kill actually
 *  happen. Two-strike (STRAY_REAP_STRIKES) so a mid-spawn channel — in the
 *  keeper but not yet in this.sessions — isn't reaped. Runs on a timer AND at
 *  the end of reconcileOpenSessions. Returns the count reaped this pass. */
export async function reapStrayKeeperChannels(this: SessionManager): Promise<number> {
	let live: Array<{ channelId: number; pid: number }>;
	try {
		live = await getMultiplexedPool().listChannelsFresh();
	} catch (e) {
		log.warn("session-manager", "stray_reap_list_failed", {
			error: String(e),
		});
		return 0;
	}
	const liveIds = new Set(live.map((c) => c.channelId));
	// Drop strike entries for channels the keeper no longer reports (exited on
	// their own) so the map can't grow unbounded.
	for (const channelId of this.strayStrikes.keys()) {
		if (!liveIds.has(channelId)) this.strayStrikes.delete(channelId);
	}
	let reaped = 0;
	for (const { channelId } of live) {
		if (this.sessions.has(channelId)) {
			this.strayStrikes.delete(channelId);
			continue;
		}
		const strikes = (this.strayStrikes.get(channelId) ?? 0) + 1;
		if (strikes < STRAY_REAP_STRIKES) {
			this.strayStrikes.set(channelId, strikes);
			continue;
		}
		this.strayStrikes.delete(channelId);
		getMultiplexedPool().kill(channelId);
		reaped++;
		log.warn("session-manager", "stray_keeper_channel_reaped", {
			channelId,
			strikes,
		});
	}
	return reaped;
}

/** Kill of a session this worker no longer holds — getBySessionId returned
 *  null because the keeper that owned the PTY was restarted (kickstart /
 *  redeploy), orphaning the row. Without this the kill silently no-ops and
 *  coord keeps the session `open` forever (unkillable). Emit a `closed`
 *  tombstone so coord projects it closed: kill is idempotent, an orphan can
 *  never get stuck open. The PTY is already gone, so closing is correct. */
export function emitClosedTombstone(
	this: SessionManager,
	sessionId: SessionId,
	reservation?: LifecycleReservation,
): void {
	const ownedReservation =
		reservation ?? this.reserveLifecycleEvent("closed");
	try {
		this.emitEvent({
			kind: "closed",
			session_id: sessionId,
			exit_code: null,
			ts: Date.now(),
		}, ownedReservation);
	} catch (error) {
		this.releaseLifecycleEvent(ownedReservation);
		throw error;
	}
}

export function kill(this: SessionManager, channelId: number): void {
	// A kill is a terminal control, and it SUPERSEDES rather than queues: the
	// session record is deleted synchronously below, so an in-flight resize
	// transaction sees a dead channel at its next await, reports ambiguous, and
	// clears its own gate/capture. Queueing the KillChild write behind a stuck
	// 6-second reconciliation would keep a user-deleted PTY alive for its whole
	// budget, and the lane records are dropped in _dropChannelState so nothing
	// queued behind it can run against a channel that no longer exists.
	const r = this.sessions.get(channelId);
	if (!r) return;
	// Mux pool sends KillChild on the shared UDS → keeper's IPty.kill →
	// child exit → Exit frame → muxCallbacks.onExit → closedByKeeper →
	// FSM closed → SessionEvent closed.
	getMultiplexedPool().kill(channelId);
	// 2026-06-15: also tear down here regardless of whether the keeper
	// still has the channel. The keeper restarts (manual kickstart,
	// crash recovery, deploy) drop their PTY children without the
	// worker noticing, so SessionRecord stays in `this.sessions` but
	// KillChild is a no-op → no Exit frame → no `closed` event → the
	// SPA's X-button does nothing because the projection never updates.
	// closedByKeeper is idempotent (FSM rejects duplicate transitions
	// and the Map.delete is no-op on missing). The keeper's own Exit
	// frame, if it does arrive, becomes a harmless second call.
	this.closedByKeeper(channelId, 0);
}

export function closedByKeeper(this: SessionManager, channelId: number, exitCode: number | null): void {
	const r = this.sessions.get(channelId);
	if (!r) return;
	this._checkDeadBirth(r);
	try {
		const transition = r.fsm.send({ kind: "close", exitCode });
		if (!transition.ok) {
			throw new Error(
				`live session ${r.sessionId} rejected close transition: ${transition.reason}`,
			);
		}
	} catch (error) {
		this.releaseLifecycleEvent(r.closeReservation);
		this._dropChannelState(channelId);
		throw error;
	}
	// _onTransition durably records the close before this state disappears.
	// @wterm/core has no dispose — WASM memory is GC'd with the bridge ref.
	this._dropChannelState(channelId);
}

/** Dead-birth self-heal: a PTY that the keeper reports exited within
 *  DEAD_BIRTH_LIFETIME_MS of spawn having produced ZERO bytes is stillborn —
 *  the degraded-survivor-keeper class (CLAUDE.md keeper-death memory),
 *  caught here from the close side. ≥KEEPER_DEAD_BIRTH_THRESHOLD within the
 *  window → fire onKeeperDegraded (grace-gated keeper restart in main.ts) so
 *  a degraded keeper replaces itself instead of rejecting "new terminal" until
 *  someone kills it by hand. head_seq===0 gate keeps a legit fast-exiting
 *  shell (which prints a prompt first) from counting. */
export function _checkDeadBirth(this: SessionManager, rec: SessionRecord): void {
	const lifetimeMs = Date.now() - rec.spawnedAtMs;
	if (lifetimeMs >= DEAD_BIRTH_LIFETIME_MS || rec.head_seq !== 0) return;
	signal("keeper.dead_birth", {
		sid: rec.sessionId,
		channel_id: rec.channelId,
		kind: rec.kind,
		lifetime_ms: lifetimeMs,
		cooldownKey: "keeper",
	});
	const now = Date.now();
	this._deadBirthBurst.push(now);
	const cutoff = now - KEEPER_DEGRADED_WINDOW_MS;
	while (this._deadBirthBurst.length && this._deadBirthBurst[0]! < cutoff)
		this._deadBirthBurst.shift();
	if (this._deadBirthBurst.length >= KEEPER_DEAD_BIRTH_THRESHOLD) {
		signal("keeper.degraded", {
			reason: "dead_births",
			dead_birth_count: this._deadBirthBurst.length,
			window_ms: KEEPER_DEGRADED_WINDOW_MS,
			cooldownKey: "keeper",
		});
		this._deadBirthBurst = []; // reset so the post-restart respawn burst can't re-fire mid-grace
		this.onKeeperDegraded?.();
	}
}

/** Mark a channel just-torn-down so its in-flight keeper frames drop silently
 *  within RECENTLY_CLOSED_TTL_MS (the post-close tail gate in
 *  emitUpstreamChunk) instead of counting toward _noSessionBurst →
 *  keeper.degraded. Shared by the session-close path and resume()'s
 *  failed-adoption teardown: both leave behind a channel whose PTY may still
 *  emit with no SessionRecord to receive the bytes. */
export function markRecentlyClosed(this: SessionManager, channelId: number): void {
	const nowMs = Date.now();
	this.recentlyClosed.set(channelId, nowMs);
	// Prune expired entries here (rare path, bounded by live channel count).
	for (const [ch, at] of this.recentlyClosed) {
		if (nowMs - at >= RECENTLY_CLOSED_TTL_MS) this.recentlyClosed.delete(ch);
	}
}

/** Remove all per-channel state when a session closes. */
export function _dropChannelState(this: SessionManager, channelId: number): void {
	const rec = this.sessions.get(channelId);
	if (rec) {
		diag("session.close", {
			sid: rec.sessionId,
			channel_id: channelId,
			session_trace_id: rec.session_trace_id,
			head_seq: rec.kind === "shell" ? rec.head_seq : null,
			kind: rec.kind,
		});
		rec.gitWatchDispose?.();
		if (rec.prPollTimer) {
			clearInterval(rec.prPollTimer);
			rec.prPollTimer = null;
		}
		if (rec.portsPollTimer) {
			clearInterval(rec.portsPollTimer);
			rec.portsPollTimer = null;
		}
		byteCapture.drop(String(rec.sessionId));
		this.onSessionClosed?.(String(rec.sessionId));
	}
	const stream = this.terminalStreams.get(channelId);
	if (stream) retireSnapshotCursor(this, channelId, stream);
	this.sessions.delete(channelId);
	this.markRecentlyClosed(channelId);
	this.terminalStreams.delete(channelId);
	this.lastAppliedSize.delete(channelId);
	this.terminalControlChains.delete(channelId);
	this.keeperAdmissionLane.delete(channelId);
	this.channelResizeSeq.delete(channelId);
	this.cellEmissionGates.delete(channelId);
	getMultiplexedPool().forgetInputSequence(channelId);
	this.hyperlinkSaturated.delete(channelId);
	const cellTimer = this.cellEmitTimers.get(channelId);
	if (cellTimer !== undefined && cellTimer !== null) {
		clearTimeout(cellTimer);
		this.cellEmitTimers.delete(channelId);
	}
	this._disposeOutputState(channelId);
}


export function _onTransition(
	this: SessionManager,
	sessionId: SessionId,
	channelId: ChannelId,
	from: ChannelState,
	to: ChannelState,
	event: FsmEvent,
): void {
	// FSM transitions are low-frequency and load-bearing for diagnosis — a
	// wedged session is read straight off these lines, so they are info, not
	// debug.
	log.info("session-manager", "fsm transition", {
		sessionId,
		channelId,
		from,
		to,
		event: event.kind,
	});
	if (to === "closed") {
		const exitCode = event.kind === "close" ? event.exitCode : null;
		const record = this.sessions.get(channelId);
		if (!record) {
			throw new Error(
				`closed transition for untracked session ${sessionId}`,
			);
		}
		this.emitEvent({
			kind: "closed",
			session_id: sessionId,
			exit_code: exitCode,
			ts: Date.now(),
		}, record.closeReservation);
	}
}


/** Stop lifecycle producers and release unconsumed eventual-close capacity.
 * Survivor keeper PTYs remain alive for the next worker process to adopt. */
export function dispose(this: SessionManager): void {
	if (this.strayReaperTimer !== null) {
		clearInterval(this.strayReaperTimer);
		this.strayReaperTimer = null;
	}
	for (const record of [...this.sessions.values()]) {
		this.releaseLifecycleEvent(record.closeReservation);
		this._dropChannelState(record.channelId);
	}
}
