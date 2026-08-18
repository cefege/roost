// Keeper reconcile/stray-reap, close/tombstone teardown, dead-birth self-heal,
// FSM transition emit, and per-channel state drop. Split out of
// session-manager.ts (400-line cap); bodies byte-for-byte unchanged, called
// with a SessionManager `this`.

import type { SessionManager } from "./session-manager.ts";
import type { SessionRecord } from "./session-record.ts";
import type { SessionId, ChannelId } from "@roost/shared";
import { log, diag, signal } from "@roost/shared";
import type { ChannelState, FsmEvent } from "./fsm.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import * as byteCapture from "./diag/byte-capture.ts";
import { cellGridEpoch, scrollbackOrigin } from "@roost/shared/cell";
import {
	ROOST_ARTIFACT_VERSION,
	ROOST_BUILD_SHA,
} from "@roost/shared/build-identity";
import {
	RECENTLY_CLOSED_TTL_MS,
	STRAY_REAP_STRIKES,
	KEEPER_DEGRADED_WINDOW_MS,
	KEEPER_DEAD_BIRTH_THRESHOLD,
	DEAD_BIRTH_LIFETIME_MS,
	SYNC_OUTPUT_MAX_MS,
	SYNC_OUTPUT_MAX_PENDING_ROWS,
} from "./session-constants.ts";
import { ringBounds, ringLength } from "./session-scrollback-ring.ts";
import { monoNowMs } from "./util/mono.ts";
import { CELL_GATE_BUDGET_MS } from "./session-resize-capture.ts";
import { unhandledSequenceSnapshot } from "./session-unhandled-seq.ts";

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
	r.fsm.send({ kind: "close", exitCode });
	// _onTransition fires the coord event.
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
	this.terminalControlChains.delete(channelId);
	this.keeperAdmissionLane.delete(channelId);
	this.channelResizeSeq.delete(channelId);
	this.resizeFloorInvalid.delete(channelId);
	this.cellEmissionGates.delete(channelId);
	this.resizeCaptures.delete(channelId);
	this.coreRebuilds.delete(channelId);
	this.hyperlinkSaturated.delete(channelId);
	const cellTimer = this.cellEmitTimers.get(channelId);
	if (cellTimer !== undefined) {
		clearTimeout(cellTimer);
		this.cellEmitTimers.delete(channelId);
	}
	this._disposeOutputState(channelId);
}

/** Truthful, on-demand state for the coordinator's diag.snapshot fan-out.
 * Every collection is bounded by the manager's live-session and existing
 * keeper-command maps; no diagnostic history is retained. */
export function diagSnapshot(this: SessionManager): Record<string, unknown> {
	const capturedAtMs = Date.now();
	// Wall clock stamps the report for an operator; every AGE below is measured
	// against this monotonic reading so a clock step cannot forge or hide a stall.
	const nowMonoMs = monoNowMs();
	const sessions: Record<string, unknown> = {};
	const keeper = getMultiplexedPool();
	for (const [channelId, rec] of this.sessions) {
		const retainedBytes = ringLength(rec.scrollback);
		const claims: Record<string, unknown> = {};
		for (const [viewerId, claim] of this.viewportClaims.get(channelId) ?? []) {
			claims[viewerId] = {
				cols: claim.cols,
				rows: claim.rows,
				last_ms: claim.lastMs,
				age_ms: Math.max(0, capturedAtMs - claim.lastMs),
				client_seq: claim.clientSeq?.toString() ?? null,
			};
		}

		const gateActive = this.cellEmissionGates.has(channelId);
		const capture = this.resizeCaptures.get(channelId);
		const suppression = this.cellGateSuppression.get(channelId);
		const syncHold = this.syncOutputHolds.get(channelId);
		const controlLane = this.terminalControlChains.get(channelId);
		const admissionLane = this.keeperAdmissionLane.get(channelId);
		let pendingResizeAcks = 0;
		let pendingResizeSeqMin: number | null = null;
		let pendingResizeSeqMax: number | null = null;
		let oldestResizeAgeMs: number | null = null;
		for (const pending of keeper.pendingResizes.values()) {
			if (pending.channelId !== channelId) continue;
			pendingResizeAcks++;
			pendingResizeSeqMin = pendingResizeSeqMin === null
				? pending.seq
				: Math.min(pendingResizeSeqMin, pending.seq);
			pendingResizeSeqMax = pendingResizeSeqMax === null
				? pending.seq
				: Math.max(pendingResizeSeqMax, pending.seq);
			const age = Math.max(0, Math.round(nowMonoMs - pending.startedMonoMs));
			if (oldestResizeAgeMs === null || age > oldestResizeAgeMs) oldestResizeAgeMs = age;
		}
		let oldestInputAgeMs: number | null = null;
		for (const pending of keeper.pendingInputs.values()) {
			if (pending.channelId !== channelId) continue;
			const age = Math.max(0, Math.round(nowMonoMs - pending.startedMonoMs));
			if (oldestInputAgeMs === null || age > oldestInputAgeMs) oldestInputAgeMs = age;
		}
		const pendingInputs = keeper._pendingInputUsage.get(channelId);
		const rawMetadata = this.rawMetadataQueues.get(channelId);
		// The core's OWN answer, read live rather than from the last emitted frame.
		// `cell.sb_dropped` below is frozen at the last SUCCESSFUL emit, so every
		// gate that withholds a frame leaves it stale by whatever the ring evicted
		// since — and comparing the two is the whole point of this block. Narrowed
		// for teardown races and sparse test fixtures, as the read paths are.
		const core = rec.wtermCore;
		const liveDropped = core ? scrollbackOrigin(core, rec.cell_emit) : null;
		const liveRetained = core ? core.getScrollbackCount() : null;
		const pin = rec.sb_origin_pin;

		sessions[String(rec.sessionId)] = {
			session_trace_id: rec.session_trace_id ?? null,
			kind: rec.kind,
			cwd: rec.cwd,
			channel_binding: {
				worker_fp: String(this.workerFp),
				channel_id: channelId,
			},
			// Three ranges that must agree and have no single place to compare them:
			// what the RING retains in bytes, what the CORE retains in lines, and
			// what the last emitted FRAME told the browser. A browser's held range
			// lands next to these in the layered probe (terminalStreamProbe).
			raw: {
				head_seq: rec.head_seq,
				tail_seq: rec.head_seq - retainedBytes,
				...ringBounds(rec.scrollback),
			},
			cell: {
				grid_epoch: cellGridEpoch(rec.cell_emit),
				seq: rec.cell_emit.seq,
				dirty: this.cellDirty.has(channelId),
				// Last EMITTED numbering. sb_origin and last_sb_total are exposed
				// separately, not just their sum: a rebuild pins the origin and a
				// stale total is what makes that pin wrong, so debugging either one
				// needs the terms, not the result.
				sb_dropped: rec.cell_emit.sbDropped,
				sb_origin: rec.cell_emit.sbOrigin,
				last_sb_total: rec.cell_emit.lastSbTotal,
				// LIVE core truth, bypassing every emission gate: the authoritative
				// range the scrollback RPCs actually serve from. `discarded` is the
				// core's own counter; `dropped`/`total` are the same fact in Roost's
				// monotonic index space, which is what a browser holds.
				core: liveDropped === null || liveRetained === null ? null : {
					discarded: liveDropped - rec.cell_emit.sbOrigin,
					dropped: liveDropped,
					retained_lines: liveRetained,
					total: liveDropped + liveRetained,
				},
				// The last core rebuild's origin pin: before/after values, whether
				// the clamp fired, and how much history the byte-ring-bounded replay
				// could not reach. null until this session has been rebuilt — and
				// nullish, not strict, because a diagnostic read must survive a
				// partial record the same way session_trace_id above does.
				origin_pin: pin == null ? null : {
					...pin,
					age_ms: Math.max(0, Math.round(nowMonoMs - pin.at_mono_ms)),
				},
			},
			gate: {
				active: gateActive || syncHold !== undefined,
				// Which gate blocked, since when, and how many frames it withheld.
				// Monotonic so a host clock step cannot fake or hide the age. A
				// synchronized-output hold that already TRIPPED still reports here
				// with over_budget=true: the emitter resumed, but the application is
				// still inside a frame it never closed, which is the fault to see.
				gate: suppression?.gate ?? (gateActive ? "resize_capture" : null),
				age_ms: suppression ? Math.max(0, Math.round(nowMonoMs - suppression.sinceMonoMs)) : null,
				suppressed_frames: suppression?.frames ?? 0,
				over_budget: suppression?.overBudget ?? false,
				budget_ms: suppression?.budgetMs ?? CELL_GATE_BUDGET_MS,
				reason: capture?.reason ?? (gateActive ? "resize_capture" : syncHold ? "sync_output" : null),
			},
			sync_output: syncHold
				? {
					generation: syncHold.generation,
					sb_total_at_open: syncHold.sbTotalAtOpen,
					tripped: syncHold.tripped,
					cap_ms: SYNC_OUTPUT_MAX_MS,
					cap_rows: SYNC_OUTPUT_MAX_PENDING_ROWS,
				}
				: null,
			resize_capture: capture
				? {
					reason: capture.reason,
					phase: capture.phase,
					phase_age_ms: Math.max(0, Math.round(nowMonoMs - capture.phaseSinceMonoMs)),
					phase_remaining_ms: Math.round(capture.phaseDeadlineMonoMs - nowMonoMs),
					txn_remaining_ms: Math.round(capture.txnDeadlineMonoMs - nowMonoMs),
					install_seq: capture.installSeq,
					boundary_seq: capture.boundarySeq >= 0 ? capture.boundarySeq : null,
					boundary_alt_mode: capture.boundaryAltMode,
					captured_bytes: capture.capturedBytes,
					captured_chunks: capture.capturedChunks,
					ring_evicted: capture.ringEvicted,
					rebuilds: capture.rebuilds,
					forwarded_replies: capture.forwardedReplies,
					over_budget: capture.overBudget,
				}
				: null,
			pending_repair: this.pendingCellRepairs.has(channelId),
			claims,
			terminal_control: {
				// Lane state is the head-of-line story: a control running while
				// writes are queued behind it is exactly what stalls input.
				control_state: controlLane?.running ?? (controlLane ? "queued" : "idle"),
				control_depth: controlLane?.depth ?? 0,
				control_running_age_ms: controlLane?.running
					? Math.max(0, Math.round(nowMonoMs - controlLane.runningSinceMonoMs))
					: null,
				admission_holder: admissionLane?.holder ?? null,
				admission_depth: admissionLane?.depth ?? 0,
				admission_held_age_ms: admissionLane?.holder
					? Math.max(0, Math.round(nowMonoMs - admissionLane.heldSinceMonoMs))
					: null,
				core_rebuilds: this.coreRebuilds.get(channelId) ?? 0,
				last_resize_seq: this.channelResizeSeq.get(channelId) ?? null,
				resize_floor_valid: !this.resizeFloorInvalid.has(channelId),
				last_applied_size: this.lastAppliedSize.get(channelId) ?? null,
				keeper_connected: Boolean(keeper.socket && !keeper.socket.destroyed),
				input_ack: {
					pending_commands: pendingInputs?.commands ?? 0,
					pending_bytes: pendingInputs?.bytes ?? 0,
					oldest_age_ms: oldestInputAgeMs,
				},
				resize_ack: {
					pending_commands: pendingResizeAcks,
					min_seq: pendingResizeSeqMin,
					max_seq: pendingResizeSeqMax,
					oldest_age_ms: oldestResizeAgeMs,
				},
				raw_metadata_queue: {
					pending_frames: rawMetadata?.frames.length ?? 0,
					pending_bytes: rawMetadata?.bytes ?? 0,
				},
			},
			terminal: {
				alt_mode: rec.alt_mode,
				cols: rec.wtermCore.getCols(),
				rows: rec.wtermCore.getRows(),
				// What the APPLICATION asked the host to do with input, read live off
				// the core. The browser gates mouse forwarding and focus reporting on
				// exactly these bits (terminalMouse.ts), so a "my clicks do nothing"
				// report is answered here instead of by guessing at the TUI's state.
				input_modes: {
					mouse_tracking: rec.wtermCore.mouseTracking?.() ?? 0,
					mouse_sgr: rec.wtermCore.mouseSgr?.() ?? false,
					focus_events: rec.wtermCore.focusEvents?.() ?? false,
				},
				// OSC 8 link table. Fixed capacity, scoped to THIS core instance
				// (a rebuild empties it). Once `saturated` is true the core keeps
				// rendering text correctly but every NEW distinct hyperlink is
				// dropped to plain text — invisible from output alone, which is
				// why the counts are reported even when nothing is wrong.
				hyperlinks: rec.wtermCore.getResourceState?.().hyperlinks ?? null,
				// Escape sequences this core reported as unhandled — the "renders wrong
				// in Roost, fine in iTerm" lane. Sampled HERE as well as on the emit
				// path so a parked pane, which produces no frames at all, still answers
				// the question. null = nothing logged, which per session-unhandled-seq.ts
				// is not proof of full support: unhandled OSC (other than 0/2/8) and
				// unimplemented DECSET/DECRST mode numbers are never logged by the core.
				unhandled_sequences: unhandledSequenceSnapshot(rec, rec.wtermCore),
			},
		};
	}
	return {
		captured_at_ms: capturedAtMs,
		build: {
			git_sha: ROOST_BUILD_SHA,
			artifact_version: ROOST_ARTIFACT_VERSION,
		},
		worker_fp: String(this.workerFp),
		keeper: {
			connected: Boolean(keeper.socket && !keeper.socket.destroyed),
			build: keeper.getRunningKeeperStamp(),
			pending_spawns: keeper.pendingSpawns.size,
			pending_list_channels: keeper.pendingListChannels.length,
			pending_history_reads: keeper.pendingGetHistory.length,
			pending_history_record_channels: keeper.pendingGetHistoryRecords.size,
			pending_history_output_channels: keeper.pendingHistoryOutput.size,
			pending_input_acks: keeper.pendingInputs.size,
			pending_resize_acks: keeper.pendingResizes.size,
		},
		sessions,
	};
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


/** Stop the class-level reaper intervals. Per-record timers clear on their
 * close paths; these run for the manager's whole lifetime. */
export function dispose(this: SessionManager): void {
	if (this.viewportReaperTimer !== null) {
		clearInterval(this.viewportReaperTimer);
		this.viewportReaperTimer = null;
	}
	if (this.strayReaperTimer !== null) {
		clearInterval(this.strayReaperTimer);
		this.strayReaperTimer = null;
	}
}
