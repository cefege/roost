// Wire dispatch for the `diag-terminal-capture` frame. Called only by
// browser-command-handler.ts; delegates to diag/terminal-capture.ts and answers
// upstream as rpc-ok with a TerminalCaptureWorkerAck.
// The coordinator already proved authorization, the single-session scope and
// the evidence bounds, so this narrows structurally and nothing more. Every
// failure is a FIXED TerminalCaptureErrorCode: an exception message can quote
// the terminal text it failed on.

import type { ClientControlFrame } from "@roost/protocol/wire";
import type { TerminalCaptureCommand } from "@roost/protocol/terminal-capture";
import type { CoordLink } from "./transport/coord-link.ts";
import type { SessionManager } from "./session-manager.ts";
import {
	captureTerminalIncident,
	startTerminalRecording,
	stopTerminalRecording,
	terminalCaptureFailureAck,
	type TerminalCaptureWorkerAck,
} from "./diag/terminal-capture.ts";

type TerminalCaptureFrame = Extract<
	ClientControlFrame,
	{ kind: "diag-terminal-capture" }
>;

export function handleDiagTerminalCapture(
	frame: TerminalCaptureFrame,
	request_id: string,
	deps: { coordLink: CoordLink; sessionMgr: SessionManager },
): void {
	const { coordLink, sessionMgr } = deps;
	const command: TerminalCaptureCommand = {
		action: frame.action,
		session_id: String(frame.session_id),
		recording_id: frame.recording_id,
		capture_id: frame.capture_id,
		reason: frame.reason,
		browser_evidence_json: frame.browser_evidence_json,
	};
	if (frame.action === "start") {
		answer(coordLink, request_id, startTerminalRecording(command, { sessionMgr }));
		return;
	}
	if (frame.action === "stop") {
		answer(coordLink, request_id, stopTerminalRecording(command));
		return;
	}
	// CAPTURE freezes synchronously inside this call and only then awaits the
	// compression and the disk write, so the PTY is never held for either.
	void captureTerminalIncident(
		{ ...command, coordinator_evidence_json: frame.coordinator_evidence_json },
		{ sessionMgr },
	).then(
		(ack) => { answer(coordLink, request_id, ack); },
		() => { answer(coordLink, request_id, terminalCaptureFailureAck("internal")); },
	);
}

function answer(
	coordLink: CoordLink,
	request_id: string,
	data: TerminalCaptureWorkerAck,
): void {
	coordLink.send({ kind: "rpc-ok", request_id, data });
}
