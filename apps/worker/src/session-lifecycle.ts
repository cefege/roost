// Keeper reconcile/stray-reap, close/tombstone teardown, dead-birth self-heal,
// FSM transition emit, agent-patch, per-channel state drop. Split out of
// session-manager.ts (400-line cap); bodies byte-for-byte unchanged, called
// with a SessionManager `this`.

import type { SessionManager } from "./session-manager.ts";
import type { SessionRecord } from "./session-record.ts";
import type { SessionId, ChannelId, AgentState } from "@roost/shared";
import { log, diag, signal } from "@roost/shared";
import type { ChannelState, FsmEvent } from "./fsm.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import * as byteCapture from "./diag/byte-capture.ts";
import * as chat from "./session-chat.ts";
import { disposeRpcChat, disposeAllRpcChats } from "./chat/omp/rpc-chat.ts";
import { forgetOmpSession } from "./chat/omp/session-store.ts";
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
export function emitClosedTombstone(this: SessionManager, sessionId: SessionId): void {
	this.emitEvent({
		kind: "closed",
		session_id: sessionId,
		exit_code: null,
		ts: Date.now(),
	});
}

export function kill(this: SessionManager, channelId: number): void {
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
	r.fsm.send({ kind: "close", exitCode });
	// _onTransition fires the coord event.
	// TRUE close (respawn deliberately bypasses this function): the pane is
	// gone for good, so drop the native chat's durable omp-session mapping.
	// _dropChannelState below only kills the child — it also runs on respawn,
	// where the same sessionId comes straight back.
	forgetOmpSession(String(r.sessionId));
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

/** Remove all per-channel maps when a session closes. The legacy
 *  sites only deleted from `this.sessions`; the SCD viewport maps
 *  (viewportClaims + lastAppliedSize) leaked across spawn/kill
 *  churn. Per-call cost: 3 Map.delete. Called from every kill /
 *  closedByKeeper / spawn-failure cleanup site so the maps never
 *  carry channels that no longer exist. */
export function _dropChannelState(this: SessionManager, channelId: number): void {
	const rec = this.sessions.get(channelId);
	if (rec) {
		diag("session.close", {
			sid: rec.sessionId,
			channel_id: channelId,
			session_trace_id: rec.session_trace_id,
			head_seq: rec.head_seq,
			kind: rec.kind,
		});
		rec.gitWatchDispose?.();
		chat._disposeChatWatch(rec);       // mirror engine: transcript tailer
		disposeRpcChat(String(rec.sessionId)); // native engine: `omp --mode rpc` child
		if (rec.prPollTimer) {
			clearInterval(rec.prPollTimer);
			rec.prPollTimer = null;
		}
		if (rec.portsPollTimer) {
			clearInterval(rec.portsPollTimer);
			rec.portsPollTimer = null;
		}
		byteCapture.drop(String(rec.sessionId));
	}
	this.sessions.delete(channelId);
	// Mark closed so post-close tail emits drop silently (see emitUpstreamChunk).
	// Prune expired entries here (rare path, bounded by live channel count).
	const nowMs = Date.now();
	this.recentlyClosed.set(channelId, nowMs);
	for (const [ch, at] of this.recentlyClosed) {
		if (nowMs - at >= RECENTLY_CLOSED_TTL_MS) this.recentlyClosed.delete(ch);
	}
	this.viewportClaims.delete(channelId);
	this.lastAppliedSize.delete(channelId);
	this._wtermRebuildChain.delete(channelId);
	const cellTimer = this.cellEmitTimers.get(channelId);
	if (cellTimer !== undefined) {
		clearTimeout(cellTimer);
		this.cellEmitTimers.delete(channelId);
	}
	const detectTimer = this.detectTimers.get(channelId);
	if (detectTimer !== undefined) {
		clearTimeout(detectTimer);
		this.detectTimers.delete(channelId);
	}
	const reevalTimer = this.reevalTimers.get(channelId);
	if (reevalTimer !== undefined) {
		clearTimeout(reevalTimer);
		this.reevalTimers.delete(channelId);
	}
	this.committedStatus.delete(channelId);
	this.lastByteAt.delete(channelId);
	this.lastOscTitle.delete(channelId);
	this.oscTitleCarry.delete(channelId);
}

/** Diag snapshot helper for diag.snapshot RPC. */
export function diagSnapshot(this: SessionManager): Record<string, unknown> {
	const sessions: Record<string, unknown> = {};
	for (const [channelId, rec] of this.sessions) {
		const claims = this.viewportClaims.get(channelId);
		sessions[String(rec.sessionId)] = {
			channel_id: channelId,
			session_trace_id: rec.session_trace_id,
			kind: rec.kind,
			cwd: rec.cwd,
			head_seq: rec.head_seq,
			tail_seq: rec.head_seq - rec.scrollback.length,
			scrollback_len: rec.scrollback.length,
			alt_mode: rec.alt_mode,
			wterm_cols: rec.wtermCore.getCols(),
			wterm_rows: rec.wtermCore.getRows(),
			last_applied_size: this.lastAppliedSize.get(channelId) ?? null,
			claims: claims ? Object.fromEntries(claims.entries()) : {},
		};
	}
	return { sessions };
}

export function _onTransition(
	this: SessionManager,
	sessionId: SessionId,
	channelId: ChannelId,
	from: ChannelState,
	to: ChannelState,
	event: FsmEvent,
): void {
	log.debug("session-manager", "fsm transition", {
		sessionId,
		channelId,
		from,
		to,
		event: event.kind,
	});
	if (to === "closed") {
		const exitCode = event.kind === "close" ? event.exitCode : null;
		this.emitEvent({
			kind: "closed",
			session_id: sessionId,
			exit_code: exitCode,
			ts: Date.now(),
		});
	}
}

/** A patch from the claude hooks (main.ts hook listener). Emits the `agent`
 *  SessionEvent with the rich fields (last_message, current_tool) AND the
 *  hook-driven status, and advances the channel FSM. Hook status must obey
 *  the STATUS CONTRACT in claude/hooks.ts (agent.status shadows the
 *  claude_status scrape in the SPA's liveStatus). */
export function applyAgentPatch(this: SessionManager, p: {
	sessionId: SessionId;
	patch: Partial<AgentState>;
}): void {
	this.emitEvent({
		kind: "agent",
		session_id: p.sessionId,
		patch: p.patch,
		ts: Date.now(),
	});
	const r = this.getBySessionId(p.sessionId);
	if (!r) return;
	const status = p.patch.status;
	if (status === "running") r.fsm.send({ kind: "agent-running" });
	else if (status === "needs-input")
		r.fsm.send({ kind: "agent-needs-input" });
	else if (status === "idle" || status === "done")
		r.fsm.send({ kind: "agent-idle" });
}

/** Stop the class-level reaper/sweep intervals. Per-record timers (git/ports
 *  polls, cell/detect/reeval debounces) clear on their own close paths; these
 *  three run for the manager's whole lifetime, so they leak the event loop
 *  unless a teardown clears them. Idempotent: nulls each handle after clearing
 *  so a double-dispose is a no-op. */
export function dispose(this: SessionManager): void {
	if (this.viewportReaperTimer !== null) {
		clearInterval(this.viewportReaperTimer);
		this.viewportReaperTimer = null;
	}
	if (this.detectSweepTimer !== null) {
		clearInterval(this.detectSweepTimer);
		this.detectSweepTimer = null;
	}
	if (this.strayReaperTimer !== null) {
		clearInterval(this.strayReaperTimer);
		this.strayReaperTimer = null;
	}
	// Native chat children are plain Bun.spawn subprocesses, NOT keeper PTY
	// channels, and _dropChannelState never runs on SIGTERM — nothing else
	// reaps them. Mappings are KEPT so the next boot resumes each conversation.
	disposeAllRpcChats();
}
