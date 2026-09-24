// The worker's reply to one `diag-terminal-capture` frame. Declared here once
// and re-exported by diag/terminal-capture.ts; the coordinator does NOT import
// it, because a worker reply crosses a trust boundary and coord narrows the
// fields structurally against the shared literal sets instead.
// Types plus the two constructors every capture path answers with.

import type {
	TerminalCaptureErrorCode,
	TerminalCaptureFileRef,
	TerminalCaptureStatus,
} from "@roost/protocol/terminal-capture";

export interface TerminalCaptureWorkerAck {
	readonly status: TerminalCaptureStatus;
	readonly path: string | null;
	readonly byte_length: number | null;
	readonly error: TerminalCaptureErrorCode | null;
	readonly expires_at_ms: number | null;
	readonly recent_worker_capture: TerminalCaptureFileRef | null;
}

/** `recent_worker_capture` names the last incident the WORKER ITSELF froze —
 *  the emission-conflict path the browser never asked for and would otherwise
 *  never learn about. The capture being answered is excluded: its own result is
 *  already `path`/`byte_length`, and echoing it there would claim the worker
 *  independently detected an incident the operator requested. */
export function recentWorkerCaptureFor(
	recent: TerminalCaptureFileRef | null,
	answeringCaptureId: string,
): TerminalCaptureFileRef | null {
	return recent !== null && recent.capture_id !== answeringCaptureId ? recent : null;
}

export function terminalCaptureFailureAck(
	error: TerminalCaptureErrorCode,
	recent: TerminalCaptureFileRef | null = null,
	expiresAtMs: number | null = null,
): TerminalCaptureWorkerAck {
	return {
		status: "error",
		path: null,
		byte_length: null,
		error,
		expires_at_ms: expiresAtMs,
		recent_worker_capture: recent,
	};
}
