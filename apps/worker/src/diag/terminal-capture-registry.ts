// The per-process registry of armed terminal-incident recorders plus the
// one-shot ledgers an unarmed manual capture needs for idempotency. Owned here
// so the lease façade (diag/terminal-capture.ts) and the writer
// (diag/terminal-capture-write.ts) share one source of truth without a cycle.
// Lease expiry is decided on SERVER time and disarms the recorder the moment it
// is observed — an expired lease is never silently renewed.

import { signal } from "@roost/observability/diag";
import { TERMINAL_CAPTURE_LIMITS } from "@roost/protocol/terminal-capture";
import {
	createCaptureLedger,
	type CaptureLedger,
	type WorkerCaptureRecorder,
} from "./terminal-capture-recorder.ts";

const _recorders = new Map<string, WorkerCaptureRecorder>();
/** Read on every retained PTY chunk, so the unarmed path settles on one
 *  integer compare before it touches a Map. */
let _armedCount = 0;
const _oneShotLedgers = new Map<string, CaptureLedger>();

export function anyRecorderArmed(): boolean {
	return _armedCount > 0;
}

export function armedRecorderCount(): number {
	return _recorders.size;
}

export function terminalRecorderArmed(sessionId: string): boolean {
	return _armedCount > 0 && activeRecorder(sessionId) !== null;
}

/** The recorder for `sessionId`, disarming it first when its server-time lease
 *  has passed. */
export function activeRecorder(sessionId: string): WorkerCaptureRecorder | null {
	const recorder = _recorders.get(sessionId);
	if (!recorder) return null;
	if (Date.now() < recorder.expires_at_ms) return recorder;
	forgetRecorder(sessionId);
	signal("terminal.capture_expired", {
		sid: sessionId,
		recording_id: recorder.recording_id,
		expires_at_ms: recorder.expires_at_ms,
		cooldownKey: recorder.recording_id,
	});
	return null;
}

export function registerRecorder(recorder: WorkerCaptureRecorder): void {
	_recorders.set(recorder.session_id, recorder);
	_armedCount = _recorders.size;
}

export function forgetRecorder(sessionId: string): void {
	_recorders.delete(sessionId);
	_armedCount = _recorders.size;
}

/** Ledger for an unarmed one-shot capture: no lease, but the same
 *  completed-capture idempotency and manual-rate obligations. */
export function oneShotLedger(sessionId: string): CaptureLedger {
	const existing = _oneShotLedgers.get(sessionId);
	if (existing) return existing;
	while (_oneShotLedgers.size >= TERMINAL_CAPTURE_LIMITS.maxRecordingsPerProcess) {
		const oldest = _oneShotLedgers.keys().next();
		if (oldest.done) break;
		_oneShotLedgers.delete(oldest.value);
	}
	const ledger = createCaptureLedger();
	_oneShotLedgers.set(sessionId, ledger);
	return ledger;
}

export function oneShotLedgerIfPresent(sessionId: string): CaptureLedger | undefined {
	return _oneShotLedgers.get(sessionId);
}

/** Session close or channel teardown: free every map, record and frozen frame
 *  this session owned. */
export function dropTerminalRecorder(sessionId: string): void {
	forgetRecorder(sessionId);
	_oneShotLedgers.delete(sessionId);
}

export function _resetTerminalCaptureRegistryForTest(): void {
	_recorders.clear();
	_oneShotLedgers.clear();
	_armedCount = 0;
}

export function _terminalRecorderForTest(
	sessionId: string,
): WorkerCaptureRecorder | undefined {
	return _recorders.get(sessionId);
}
