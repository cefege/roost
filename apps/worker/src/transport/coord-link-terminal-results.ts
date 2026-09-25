// Typed worker terminal-control result shaping for CoordLink callbacks. It keeps
// the pre-write/ambiguous truth mapping in one place so direct work-budget
// refusals, Sync input, and agent prompts never manufacture retry permission.
// Reasons are bounded only where an agent-originated result could carry details.

import {
	TerminalInputStatus,
	TerminalStreamFailureKind,
	TerminalWritePhase,
} from "@roost/protocol/proto/worker_transport_pb";
import type { TerminalStreamFailure } from "../session/session-terminal-state.ts";
import type { WorkerInputResult } from "../session/session-terminal-control.ts";
import type { UpstreamFrame } from "./coord-link-types.ts";

export function terminalStreamFailureKind(
	failure: TerminalStreamFailure | undefined,
): TerminalStreamFailureKind {
	switch (failure) {
		case "retryable_pre_write": return TerminalStreamFailureKind.RETRYABLE_PRE_WRITE;
		case "session_not_live": return TerminalStreamFailureKind.SESSION_NOT_LIVE;
		case "invalid_request": return TerminalStreamFailureKind.INVALID_REQUEST;
		case "core_failed": return TerminalStreamFailureKind.CORE_FAILED;
		case "ambiguous_boundary": return TerminalStreamFailureKind.AMBIGUOUS_BOUNDARY;
		default: return TerminalStreamFailureKind.UNSPECIFIED;
	}
}

export function boundedTerminalReason(reason: string | undefined): string | undefined {
	if (!reason) return undefined;
	const encoded = Buffer.from(reason);
	if (encoded.byteLength <= 200) return reason;
	for (let end = 200; end > 0; end -= 1) {
		try {
			return new TextDecoder("utf-8", { fatal: true }).decode(encoded.subarray(0, end));
		} catch {
			// Continue to the previous UTF-8 boundary.
		}
	}
	return "";
}

export function sendTerminalInputResult(
	send: (frame: UpstreamFrame) => boolean,
	request: { requestId: string; sessionId: string; inputSeq: bigint },
	result: WorkerInputResult,
	boundReason: boolean,
): void {
	send({
		kind: "input-result",
		request_id: request.requestId,
		session_id: request.sessionId,
		input_seq: request.inputSeq,
		status: result.status === "accepted"
			? TerminalInputStatus.ACCEPTED
			: result.status === "rejected"
				? TerminalInputStatus.REJECTED
				: TerminalInputStatus.AMBIGUOUS,
		written_bytes: result.writtenBytes,
		phase: result.status === "accepted"
			? TerminalWritePhase.WRITTEN
			: result.status === "rejected"
				? TerminalWritePhase.PRE_WRITE
				: TerminalWritePhase.UNKNOWN,
		reason: result.status === "accepted"
			? undefined
			: (boundReason ? boundedTerminalReason(result.reason) : result.reason),
	});
}
