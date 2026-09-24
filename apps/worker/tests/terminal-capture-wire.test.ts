// Worker terminal-incident capture wiring. Verify:
//   - the REAL emitCellFrame path feeds the recorder at accepted emissions
//     only, and a healthy full+delta pair compares equal and stays silent
//   - a dropped delta on the real path invalidates the fold
//   - the diag-terminal-capture frame handler answers rpc-ok with the worker
//     ack for start / capture / stop, and never an exception message

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import type { ClientControlFrame, CoordWorkerUpstream } from "@roost/protocol/wire";
import { handleDiagTerminalCapture } from "../src/browser-command-terminal-capture.ts";
import type { CoordLink } from "../src/transport/coord-link.ts";
import { _resetCaptureStorageForTest } from "../src/diag/capture-storage.ts";
import {
	startTerminalRecording,
	_resetTerminalCaptureForTest,
	_settleScheduledCaptures,
	_terminalRecorderForTest,
} from "../src/diag/terminal-capture.ts";
import {
	cleanupStreamHarnesses,
	enableStream,
	flushLeadingCellEmit,
	makeHarness,
	paintRows,
	SESSION_ID,
	CHANNEL_ID,
	STREAM_A,
} from "./terminal-stream-state-harness.ts";
import { FIXTURE_RECORDING_ID } from "./terminal-capture-fixtures.ts";

const START_COMMAND = {
	action: "start",
	session_id: String(SESSION_ID),
	recording_id: FIXTURE_RECORDING_ID,
	capture_id: "eeeeeeee-0000-4000-8000-00000000ee03",
	reason: "manual",
	browser_evidence_json: "",
} as const;

let captureRoot: string;
const priorLogDir = process.env.ROOST_WORKER_LOG_DIR;

beforeEach(() => {
	_resetTerminalCaptureForTest();
	_resetCaptureStorageForTest();
	captureRoot = mkdtempSync(join(tmpdir(), "roost-capture-wire-"));
	process.env.ROOST_WORKER_LOG_DIR = join(captureRoot, "RoostWorker");
});

afterEach(() => {
	cleanupStreamHarnesses();
	_resetTerminalCaptureForTest();
	_resetCaptureStorageForTest();
	if (priorLogDir === undefined) delete process.env.ROOST_WORKER_LOG_DIR;
	else process.env.ROOST_WORKER_LOG_DIR = priorLogDir;
	try { rmSync(captureRoot, { recursive: true, force: true }); } catch { /* ignore */ }
});

describe("worker terminal capture wiring", () => {
	test("the live emitter records accepted frames and stays silent when healthy", async () => {
		const harness = await makeHarness();
		await enableStream(harness.manager, STREAM_A);
		await flushLeadingCellEmit();
		paintRows(harness.record.wtermCore, ["FOOTER-12s"]);
		startTerminalRecording(START_COMMAND, { sessionMgr: harness.manager });

		harness.manager.emitCellFrame(CHANNEL_ID, true);
		const recorder = _terminalRecorderForTest(String(SESSION_ID))!;
		// Force the second emission to be sampled too, so "silent" is not just
		// "unsampled".
		recorder.last_sample_mono_ms = Number.NEGATIVE_INFINITY;
		harness.record.wtermCore.writeString("\x1b[2;1HFOOTER-14s");
		harness.manager.emitCellFrame(CHANNEL_ID, false);
		await _settleScheduledCaptures();

		const emitted = recorder.emissions.map((entry) => ({
			full: entry.record.full,
			comparison: entry.record.comparison,
		}));
		expect(emitted).toEqual([
			{ full: true, comparison: "equal" },
			{ full: false, comparison: "equal" },
		]);
		expect(recorder.fold).not.toBeNull();
		expect(recorder.sampling.sampled).toBe(2);
	});

	test("a dropped delta on the live path invalidates the fold", async () => {
		let dropDeltas = false;
		const harness = await makeHarness(undefined, {
			sendFrame: (frame) => (dropDeltas && !frame.full ? "dropped" : "sent"),
		});
		await enableStream(harness.manager, STREAM_A);
		await flushLeadingCellEmit();
		startTerminalRecording(START_COMMAND, { sessionMgr: harness.manager });
		harness.manager.emitCellFrame(CHANNEL_ID, true);
		const recorder = _terminalRecorderForTest(String(SESSION_ID))!;
		expect(recorder.fold).not.toBeNull();

		dropDeltas = true;
		harness.record.wtermCore.writeString("\x1b[3;1Hdropped");
		harness.manager.emitCellFrame(CHANNEL_ID, false);

		// The dropped delta was never retained as evidence, and the repair full
		// the emitter then requests re-establishes the baseline it invalidated.
		expect([...recorder.fold_reasons]).toContain("baseline_invalidated");
		expect(recorder.emissions.every((entry) => entry.record.full)).toBe(true);
	});

	test("the capture frame handler answers rpc-ok for start, capture and stop", async () => {
		const harness = await makeHarness();
		await enableStream(harness.manager, STREAM_A);
		const link = replyRecordingLink();

		handleDiagTerminalCapture(
			captureFrame({ action: "start", request_id: "req-start" }),
			"req-start",
			{ coordLink: link.coordLink, sessionMgr: harness.manager },
		);
		expect(await link.reply("req-start")).toMatchObject({ status: "recording", error: null });

		handleDiagTerminalCapture(
			captureFrame({ action: "capture", request_id: "req-capture" }),
			"req-capture",
			{ coordLink: link.coordLink, sessionMgr: harness.manager },
		);
		const captureAck = await link.reply("req-capture");
		expect(captureAck.status).toBe("partial");
		expect(captureAck.error).toBeNull();
		expect(String(captureAck.path)).toContain("terminal-incident-");
		expect(String(captureAck.path).length).toBeLessThanOrEqual(1024);

		handleDiagTerminalCapture(
			captureFrame({ action: "stop", request_id: "req-stop" }),
			"req-stop",
			{ coordLink: link.coordLink, sessionMgr: harness.manager },
		);
		expect(await link.reply("req-stop")).toMatchObject({ status: "stopped", error: null });
		expect(_terminalRecorderForTest(String(SESSION_ID))).toBeUndefined();
	});

	test("malformed browser evidence is refused with a fixed code", async () => {
		const harness = await makeHarness();
		await enableStream(harness.manager, STREAM_A);
		const link = replyRecordingLink();

		handleDiagTerminalCapture(
			captureFrame({
				action: "capture",
				request_id: "req-bad",
				browser_evidence_json: "{not json",
			}),
			"req-bad",
			{ coordLink: link.coordLink, sessionMgr: harness.manager },
		);
		expect(await link.reply("req-bad")).toMatchObject({
			status: "error",
			error: "evidence_malformed",
			path: null,
		});
	});
});

type TerminalCaptureFrame = Extract<ClientControlFrame, { kind: "diag-terminal-capture" }>;

function captureFrame(
	overrides: Partial<TerminalCaptureFrame> & { readonly action: "start" | "capture" | "stop" },
): TerminalCaptureFrame {
	return {
		kind: "diag-terminal-capture",
		request_id: "req",
		session_id: SESSION_ID,
		recording_id: FIXTURE_RECORDING_ID,
		capture_id: "eeeeeeee-0000-4000-8000-00000000ee04",
		reason: "manual",
		browser_evidence_json: "",
		coordinator_evidence_json: "",
		...overrides,
	} as TerminalCaptureFrame;
}

interface WireAck {
	readonly status: string;
	readonly error: string | null;
	readonly path: string | null;
}

interface ReplyRecordingLink {
	readonly coordLink: CoordLink;
	/** Resolves on the ACTUAL upstream answer for `requestId`, past or future —
	 *  the only truthful "the worker got here" signal for a fire-and-forget
	 *  handler. */
	reply(requestId: string): Promise<WireAck>;
}

function replyRecordingLink(): ReplyRecordingLink {
	const answered = new Map<string, WireAck>();
	const waiting = new Map<string, (ack: WireAck) => void>();
	const coordLink = {
		send: (frame: CoordWorkerUpstream) => {
			if (frame.kind !== "rpc-ok") {
				throw new Error(`capture handler must answer rpc-ok, got ${frame.kind}`);
			}
			const ack = frame.data as WireAck;
			answered.set(frame.request_id, ack);
			waiting.get(frame.request_id)?.(ack);
		},
	} as unknown as CoordLink;
	return {
		coordLink,
		reply: (requestId) => {
			const existing = answered.get(requestId);
			if (existing) return Promise.resolve(existing);
			return new Promise<WireAck>((resolve) => { waiting.set(requestId, resolve); });
		},
	};
}
