// Truthful, on-demand state for the coordinator's diag.snapshot fan-out,
// extracted verbatim from session-lifecycle.ts (pure read-only formatter over
// the manager's live maps — no mutation, no retained history). Every AGE is
// measured against a single monotonic reading so a host clock step cannot
// forge or hide a stall. Called by SessionManager.diagSnapshot().

import type { SessionManager } from "./session-manager.ts";
import type { SessionRecord } from "./session-record.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import { cellGridEpoch, scrollbackOrigin } from "@roost/shared/cell";
import {
	ROOST_ARTIFACT_VERSION,
	ROOST_BUILD_SHA,
} from "@roost/shared/build-identity";
import {
	SYNC_OUTPUT_MAX_MS,
	SYNC_OUTPUT_MAX_PENDING_ROWS,
} from "./session-constants.ts";
import { ringBounds, ringLength } from "./session-scrollback-ring.ts";
import { monoNowMs } from "./util/mono.ts";
import { CELL_GATE_BUDGET_MS } from "./session-resize-capture.ts";
import { unhandledSequenceSnapshot } from "./session-unhandled-seq.ts";

export function diagSnapshot(this: SessionManager): Record<string, unknown> {
	const capturedAtMs = Date.now();
	// Wall clock stamps the report for an operator; every AGE below is measured
	// against this monotonic reading so a clock step cannot forge or hide a stall.
	const nowMonoMs = monoNowMs();
	const sessions: Record<string, unknown> = {};
	const keeper = getMultiplexedPool();
	for (const [channelId, rec] of this.sessions) {
		const record = rec as SessionRecord;
		const retainedBytes = ringLength(record.scrollback);

		const gateActive = this.cellEmissionGates.has(channelId);
		const stream = this.terminalStreams.get(channelId);
		const capture = stream?.resizeCapture ?? null;
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
		const core = record.wtermCore;
		const liveDropped = core ? scrollbackOrigin(core, record.cell_emit) : null;
		const liveRetained = core ? core.getScrollbackCount() : null;
		const pin = record.sb_origin_pin;

		sessions[String(record.sessionId)] = {
			session_trace_id: record.session_trace_id ?? null,
			kind: record.kind,
			cwd: record.cwd,
			channel_binding: {
				worker_fp: String(this.workerFp),
				channel_id: channelId,
			},
			// Three ranges that must agree and have no single place to compare them:
			// what the RING retains in bytes, what the CORE retains in lines, and
			// what the last emitted FRAME told the browser. A browser's held range
			// lands next to these in the layered probe (terminalStreamProbe).
			raw: {
				head_seq: record.head_seq,
				tail_seq: record.head_seq - retainedBytes,
				...ringBounds(record.scrollback),
			},
			cell: {
				grid_epoch: cellGridEpoch(record.cell_emit),
				seq: record.cell_emit.seq,
				dirty: this.cellDirty.has(channelId),
				// Last EMITTED numbering. sb_origin and last_sb_total are exposed
				// separately, not just their sum: a rebuild pins the origin and a
				// stale total is what makes that pin wrong, so debugging either one
				// needs the terms, not the result.
				sb_dropped: record.cell_emit.sbDropped,
				sb_origin: record.cell_emit.sbOrigin,
				last_sb_total: record.cell_emit.lastSbTotal,
				// LIVE core truth, bypassing every emission gate: the authoritative
				// range the scrollback RPCs actually serve from. `discarded` is the
				// core's own counter; `dropped`/`total` are the same fact in Roost's
				// monotonic index space, which is what a browser holds.
				core: liveDropped === null || liveRetained === null ? null : {
					discarded: liveDropped - record.cell_emit.sbOrigin,
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
				reason: capture?.failedReason ?? (gateActive ? "resize_capture" : syncHold ? "sync_output" : null),
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
					stream_id: capture.streamId,
					resize_seq: capture.resizeSeq,
					install_seq: capture.installSeq,
					from_cols: capture.fromCols,
					from_rows: capture.fromRows,
					to_cols: capture.toCols,
					to_rows: capture.toRows,
					boundary_seq: capture.boundarySeq >= 0 ? capture.boundarySeq : null,
					boundary_applied: capture.boundaryApplied,
					captured_bytes: capture.capturedBytes,
					captured_chunks: capture.capturedChunks,
					failed_reason: capture.failedReason,
				}
				: null,
			pending_repair: this.pendingCellRepairs.has(channelId),
			terminal_stream: stream
				? {
					stream_id: stream.streamId,
					enabled: stream.enabled,
					cols: stream.cols,
					rows: stream.rows,
					baseline_ready: stream.baselineReady,
					baseline_dirty: stream.baselineDirty,
					core_valid: stream.coreValid,
					snapshot_id: stream.snapshotCursor?.snapshotId ?? null,
					snapshot_next_part: stream.snapshotCursor?.nextPart ?? null,
					snapshot_part_count: stream.snapshotCursor?.parts.length ?? null,
				}
				: null,
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
				last_resize_seq: this.channelResizeSeq.get(channelId) ?? null,
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
				alt_mode: record.alt_mode,
				cols: record.wtermCore.getCols(),
				rows: record.wtermCore.getRows(),
				// What the APPLICATION asked the host to do with input, read live off
				// the core. The browser gates mouse forwarding and focus reporting on
				// exactly these bits (terminalMouse.ts), so a "my clicks do nothing"
				// report is answered here instead of by guessing at the TUI's state.
				input_modes: {
					mouse_tracking: record.wtermCore.mouseTracking?.() ?? 0,
					mouse_sgr: record.wtermCore.mouseSgr?.() ?? false,
					focus_events: record.wtermCore.focusEvents?.() ?? false,
				},
				// OSC 8 link table. Fixed capacity, scoped to THIS core instance
				// (a rebuild empties it). Once `saturated` is true the core keeps
				// rendering text correctly but every NEW distinct hyperlink is
				// dropped to plain text — invisible from output alone, which is
				// why the counts are reported even when nothing is wrong.
				hyperlinks: record.wtermCore.getResourceState?.().hyperlinks ?? null,
				// Escape sequences this core reported as unhandled — the "renders wrong
				// in Roost, fine in iTerm" lane. Sampled HERE as well as on the emit
				// path so a parked pane, which produces no frames at all, still answers
				// the question. null = nothing logged, which per session-unhandled-seq.ts
				// is not proof of full support: unhandled OSC (other than 0/2/8) and
				// unimplemented DECSET/DECRST mode numbers are never logged by the core.
				unhandled_sequences: unhandledSequenceSnapshot(record, record.wtermCore),
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
