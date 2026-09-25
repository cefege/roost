// Worker façade for opt-in terminal incident capture: the lease semantics a
// START/STOP carries, and the four guarded taps the terminal data path calls.
// Called by browser-command-terminal-capture.ts (wire), session-scrollback.ts
// (raw bytes), session-resize-capture.ts (geometry boundaries), session-emit.ts
// (accepted emissions) and session-lifecycle.ts (teardown).
// Registry state lives in terminal-capture-registry.ts, bundle assembly in
// terminal-capture-write.ts; every bound comes from TERMINAL_CAPTURE_LIMITS.

import { cellGridEpoch, type CellGridFrame } from "@roost/protocol/cell";
import { signal } from "@roost/observability/diag";
import {
	TERMINAL_CAPTURE_LIMITS,
	type TerminalCaptureCommand,
	type TerminalCoverageReason,
	type TerminalWorkerResizeOutcome,
} from "@roost/protocol/terminal-capture";
import type { SessionManager } from "../session/session-manager.ts";
import type { SessionRecord } from "../session/session-record.ts";
import type { LiveResizeCapture, TerminalStreamState } from "../session/session-terminal-state.ts";
import { ensureCaptureRetention, stopCaptureRetention } from "./capture-storage.ts";
import {
	recentWorkerCaptureFor,
	terminalCaptureFailureAck,
	type TerminalCaptureWorkerAck,
} from "./terminal-capture-ack.ts";
import { recordAcceptedEmission } from "./terminal-capture-emission.ts";
import {
	RAW_POOL,
	rawRecordBytes,
	retainResizeRecord,
	retainWorkerRecord,
} from "./terminal-capture-pools.ts";
import {
	activeRecorder,
	anyRecorderArmed,
	armedRecorderCount,
	forgetRecorder,
	oneShotLedgerIfPresent,
	registerRecorder,
	_resetTerminalCaptureRegistryForTest,
} from "./terminal-capture-registry.ts";
import {
	createWorkerCaptureRecorder,
	invalidateWorkerFold,
	latchAutomaticCapture,
	openWorkerSegment,
	type WorkerCaptureRecorder,
} from "./terminal-capture-recorder.ts";
import {
	scheduleWorkerLocalCapture,
	type TerminalCaptureWorkerDeps,
} from "./terminal-capture-write.ts";

export { terminalCaptureFailureAck } from "./terminal-capture-ack.ts";
export type { TerminalCaptureWorkerAck } from "./terminal-capture-ack.ts";
export type { WorkerCaptureRecorder } from "./terminal-capture-recorder.ts";
export {
	dropTerminalRecorder,
	terminalRecorderArmed,
	_terminalRecorderForTest,
} from "./terminal-capture-registry.ts";
export {
	captureTerminalIncident,
	_settleScheduledCaptures,
	type TerminalCaptureWorkerCommand,
	type TerminalCaptureWorkerDeps,
} from "./terminal-capture-write.ts";

/** Arm the worker recorder. A repeat from the SAME recording renews the lease
 *  and preserves every retained record; a different recording on a live lease
 *  is a conflict, because evicting another operator's evidence is never the
 *  right answer to a second request. */
export function startTerminalRecording(
	command: TerminalCaptureCommand,
	deps: TerminalCaptureWorkerDeps,
): TerminalCaptureWorkerAck {
	const nowMs = Date.now();
	const existing = activeRecorder(command.session_id);
	if (existing) {
		if (existing.recording_id !== command.recording_id) {
			return terminalCaptureFailureAck(
				"lease_conflict",
				recentWorkerCaptureFor(existing.ledger.recent_worker_local, command.capture_id),
				existing.expires_at_ms,
			);
		}
		existing.expires_at_ms = nowMs + TERMINAL_CAPTURE_LIMITS.leaseMs;
		return recordingAck(existing, command.capture_id);
	}
	const record = deps.sessionMgr.getBySessionId(command.session_id);
	if (!record) return terminalCaptureFailureAck("session_unknown");
	if (armedRecorderCount() >= TERMINAL_CAPTURE_LIMITS.maxRecordingsPerProcess) {
		return terminalCaptureFailureAck("resource_exhausted");
	}
	const recorder = createWorkerCaptureRecorder({
		session_id: command.session_id,
		worker_fp: String(deps.sessionMgr.workerFp),
		recording_id: command.recording_id,
		expires_at_ms: nowMs + TERMINAL_CAPTURE_LIMITS.leaseMs,
		at_ms: nowMs,
		head_seq: record.head_seq,
	});
	// Open the first segment now, so raw bytes arriving before the next emission
	// still land in an orderable generation.
	const stream = deps.sessionMgr.terminalStreams.get(record.channelId);
	openWorkerSegment(recorder, {
		stream_id: stream?.streamId ?? record.cell_emit.streamId,
		stream_version: stream?.version ?? 0,
		grid_epoch: cellGridEpoch(record.cell_emit),
		grid_epoch_base: record.cell_emit.gridEpochBase,
		geometry: { cols: record.wtermCore.getCols(), rows: record.wtermCore.getRows() },
		head_seq: record.head_seq,
		at_ms: nowMs,
	});
	registerRecorder(recorder);
	ensureCaptureRetention();
	signal("terminal.capture_started", {
		sid: command.session_id,
		recording_id: command.recording_id,
		expires_at_ms: recorder.expires_at_ms,
		armed_offset: recorder.armed_offset,
		cooldownKey: command.recording_id,
	});
	return recordingAck(recorder, command.capture_id);
}

/** Release the lease and free every retained record. Saved incident files are
 *  NOT deleted, and a repeat STOP by the owner is harmless. */
export function stopTerminalRecording(
	command: TerminalCaptureCommand,
): TerminalCaptureWorkerAck {
	const recorder = activeRecorder(command.session_id);
	if (!recorder) {
		const oneShot = oneShotLedgerIfPresent(command.session_id);
		return stoppedAck(
			recentWorkerCaptureFor(oneShot?.recent_worker_local ?? null, command.capture_id),
		);
	}
	if (recorder.recording_id !== command.recording_id) {
		return terminalCaptureFailureAck("permission_denied", null, recorder.expires_at_ms);
	}
	const recent = recentWorkerCaptureFor(
		recorder.ledger.recent_worker_local,
		command.capture_id,
	);
	forgetRecorder(command.session_id);
	signal("terminal.capture_stopped", {
		sid: command.session_id,
		recording_id: command.recording_id,
		cooldownKey: command.recording_id,
	});
	return stoppedAck(recent);
}

/** Process shutdown: the retention timer is the only thing here that outlives
 *  a session. */
export function stopTerminalCaptureMaintenance(): void {
	stopCaptureRetention();
}

/** One retained PTY chunk with its exact absolute bounds. Called from
 *  session-scrollback.ts::retainRaw AFTER the always-on byte ring push, so the
 *  live and capture lanes retain the identical window. */
export function noteRetainedRawChunk(
	sessionId: string,
	endSeq: number,
	chunk: Uint8Array,
): void {
	if (!anyRecorderArmed()) return;
	const recorder = activeRecorder(sessionId);
	const segment = recorder?.open_segment;
	if (!recorder || !segment) return;
	const startOffset = Math.max(0, endSeq - chunk.byteLength);
	const range = { start: String(startOffset), end: String(endSeq) };
	retainWorkerRecord(
		recorder,
		recorder.raw,
		RAW_POOL,
		{
			segment_id: segment.segment_id,
			at_ms: Date.now(),
			start_offset: range.start,
			end_offset: range.end,
			// Buffer.from COPIES a view: the ring overwrites in place, and a
			// retained view would decode as whatever arrived next.
			base64: Buffer.from(chunk).toString("base64"),
		},
		rawRecordBytes(chunk.byteLength),
		range,
	);
}

/** A sequenced resize gate opened. The record is completed by
 *  noteResizeResult once the keeper proves (or fails to prove) the boundary. */
export function noteResizeInstall(
	mgr: SessionManager,
	channelId: number,
	capture: LiveResizeCapture,
): void {
	if (!anyRecorderArmed()) return;
	const record = mgr.sessions.get(channelId);
	if (!record) return;
	const recorder = activeRecorder(String(record.sessionId));
	const segment = recorder?.open_segment;
	if (!recorder || !segment) return;
	retainResizeRecord(recorder, {
		segment_id: segment.segment_id,
		at_ms: Date.now(),
		resize_seq: capture.resizeSeq,
		install_offset: String(capture.installSeq),
		boundary_offset: null,
		from: { cols: capture.fromCols, rows: capture.fromRows },
		to: { cols: capture.toCols, rows: capture.toRows },
		outcome: "unknown",
		grid_epoch_before: cellGridEpoch(record.cell_emit),
		grid_epoch_after: null,
		captured_bytes: 0,
	});
}

/** The keeper-acknowledged parse boundary, or its absence. `boundarySeq` is the
 *  raw offset the ACK (or the ordered history recovery) landed at — never a
 *  request time, which proves nothing about where the new geometry applied. */
export function noteResizeResult(
	mgr: SessionManager,
	channelId: number,
	capture: LiveResizeCapture,
	outcome: TerminalWorkerResizeOutcome,
): void {
	if (!anyRecorderArmed()) return;
	const record = mgr.sessions.get(channelId);
	if (!record) return;
	const recorder = activeRecorder(String(record.sessionId));
	if (!recorder) return;
	const install = String(capture.installSeq);
	for (let idx = recorder.resizes.length - 1; idx >= 0; idx -= 1) {
		const resize = recorder.resizes[idx]!;
		if (resize.resize_seq !== capture.resizeSeq || resize.install_offset !== install) continue;
		resize.outcome = outcome;
		resize.captured_bytes = capture.capturedBytes;
		resize.grid_epoch_after = cellGridEpoch(record.cell_emit);
		resize.boundary_offset = capture.boundaryApplied && capture.boundarySeq >= 0
			? String(capture.boundarySeq)
			: null;
		return;
	}
}

/** One ACCEPTED cell emission. Latches and schedules at most one worker-local
 *  capture per (recording, stream, epoch, reason), under the session-wide
 *  automatic floor. */
export function noteAcceptedCellEmission(
	record: SessionRecord,
	stream: TerminalStreamState,
	frame: CellGridFrame,
): void {
	if (!anyRecorderArmed()) return;
	const recorder = activeRecorder(String(record.sessionId));
	if (!recorder) return;
	const conflict = recordAcceptedEmission(recorder, record, stream, frame);
	if (!conflict) return;
	const nowMs = Date.now();
	const latchKey =
		`${recorder.recording_id}|${conflict.stream_id}|${conflict.grid_epoch}|worker_emission`;
	if (!latchAutomaticCapture(recorder, latchKey, nowMs)) return;
	const difference = conflict.difference;
	signal("terminal.emission_conflict", {
		sid: recorder.session_id,
		stream_id: conflict.stream_id,
		grid_epoch: conflict.grid_epoch,
		seq: conflict.seq,
		field: difference.field,
		row: difference.kind === "row" ? difference.row : null,
		column: difference.kind === "row" ? difference.column : null,
		cooldownKey: latchKey,
	});
	scheduleWorkerLocalCapture(
		recorder,
		record,
		stream,
		{ seq: conflict.seq, latch_key: latchKey },
		nowMs,
	);
}

/** A dropped or rejected frame: the fold can no longer reproduce the shipped
 *  screen, so it is invalid until the next accepted full. */
export function noteRejectedCellEmission(
	record: SessionRecord,
	reason: TerminalCoverageReason,
): void {
	if (!anyRecorderArmed()) return;
	const recorder = activeRecorder(String(record.sessionId));
	if (recorder) invalidateWorkerFold(recorder, reason);
}

export function _resetTerminalCaptureForTest(): void {
	_resetTerminalCaptureRegistryForTest();
	stopCaptureRetention();
}

function recordingAck(
	recorder: WorkerCaptureRecorder,
	captureId: string,
): TerminalCaptureWorkerAck {
	return {
		status: "recording",
		path: null,
		byte_length: null,
		error: null,
		expires_at_ms: recorder.expires_at_ms,
		recent_worker_capture: recentWorkerCaptureFor(
			recorder.ledger.recent_worker_local,
			captureId,
		),
	};
}

function stoppedAck(
	recent: TerminalCaptureWorkerAck["recent_worker_capture"],
): TerminalCaptureWorkerAck {
	return {
		status: "stopped",
		path: null,
		byte_length: null,
		error: null,
		expires_at_ms: null,
		recent_worker_capture: recent,
	};
}
