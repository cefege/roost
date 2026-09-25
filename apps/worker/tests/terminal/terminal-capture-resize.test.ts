// Worker terminal-incident resize + capture-concurrency behavior. Verify:
//   - an accepted resize records the KEEPER-acknowledged parse boundary offset
//     and the epoch transition, not a request timestamp
//   - a lost ACK still leaves a retained record, with a null boundary and a
//     coverage reason that says core replay is not complete
//   - a capture taken while a resize boundary is unresolved reads the frozen
//     core directly and issues NO keeper history request, which would contend
//     with the lost-ACK recovery path for the same history slot

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { gunzipSync } from "node:zlib";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { getMultiplexedPool } from "../../src/keeper/multiplexed-client.ts";
import { _resetCaptureStorageForTest } from "../../src/diag/capture-storage.ts";
import {
	captureTerminalIncident,
	noteResizeInstall,
	noteResizeResult,
	startTerminalRecording,
	_resetTerminalCaptureForTest,
	_terminalRecorderForTest,
} from "../../src/diag/terminal-capture.ts";
import { installLiveResizeCapture } from "../../src/session/session-resize-capture.ts";
import type { LiveResizeCapture } from "../../src/session/session-terminal-state.ts";
import { installAutoKeeper } from "../keeper-fake-pool.ts";
import {
	cleanupStreamHarnesses,
	enableStream,
	makeHarness,
	SESSION_ID,
	CHANNEL_ID,
	STREAM_A,
	STREAM_B,
	TEST_COLS,
	TEST_ROWS,
	trackKeeper,
} from "./terminal-stream-state-harness.ts";
import { captureCommandFixture, FIXTURE_RECORDING_ID } from "./terminal-capture-fixtures.ts";

const START_COMMAND = {
	action: "start",
	session_id: String(SESSION_ID),
	recording_id: FIXTURE_RECORDING_ID,
	capture_id: "eeeeeeee-0000-4000-8000-00000000ee02",
	reason: "manual",
	browser_evidence_json: "",
} as const;

let captureRoot: string;
const priorLogDir = process.env.ROOST_WORKER_LOG_DIR;

beforeEach(() => {
	_resetTerminalCaptureForTest();
	_resetCaptureStorageForTest();
	captureRoot = mkdtempSync(join(tmpdir(), "roost-capture-resize-"));
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

describe("worker terminal capture resize boundaries", () => {
	test("an accepted resize records the keeper-acknowledged boundary offset", async () => {
		trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
		const harness = await makeHarness();
		await enableStream(harness.manager, STREAM_A);
		expect(startTerminalRecording(START_COMMAND, { sessionMgr: harness.manager }).status)
			.toBe("recording");
		const epochBefore = harness.record.cell_emit.gridEpochBase;

		const resized = await enableStream(harness.manager, STREAM_B, 10, 3);
		expect(resized).toMatchObject({ status: "committed", resized: true });

		const recorder = _terminalRecorderForTest(String(SESSION_ID))!;
		expect(recorder.resizes).toHaveLength(1);
		const resize = recorder.resizes[0]!;
		expect(resize.outcome).toBe("accepted");
		// The boundary is the raw offset the ACK landed at, so with no output in
		// flight it equals the install offset — and it is never null.
		expect(resize.boundary_offset).toBe(String(harness.record.head_seq));
		expect(resize.boundary_offset).toBe(resize.install_offset);
		expect(resize.from).toEqual({ cols: TEST_COLS, rows: TEST_ROWS });
		expect(resize.to).toEqual({ cols: 10, rows: 3 });
		expect(resize.grid_epoch_before.startsWith(epochBefore)).toBe(true);
		expect(resize.grid_epoch_after).not.toBe(resize.grid_epoch_before);
	});

	test("a lost ACK retains the record with no proven boundary", async () => {
		const harness = await makeHarness();
		await enableStream(harness.manager, STREAM_A);
		startTerminalRecording(START_COMMAND, { sessionMgr: harness.manager });

		const capture = unresolvedCapture(harness.record.head_seq);
		noteResizeInstall(harness.manager, CHANNEL_ID, capture);
		noteResizeResult(harness.manager, CHANNEL_ID, capture, "lost_ack");

		const recorder = _terminalRecorderForTest(String(SESSION_ID))!;
		expect(recorder.resizes).toHaveLength(1);
		expect(recorder.resizes[0]!.outcome).toBe("lost_ack");
		expect(recorder.resizes[0]!.boundary_offset).toBeNull();

		const result = await captureTerminalIncident(
			captureCommandFixture({ session_id: String(SESSION_ID) }),
			{ sessionMgr: harness.manager },
		);
		expect(result.status).toBe("partial");
		const bundle = readBundle(result.path!);
		expect(bundle.coverage.core_replay).toBe("partial");
		expect(bundle.coverage.core_replay_reasons).toContain("missing_resize_boundary");
		expect(bundle.worker.resizes[0]!.outcome).toBe("lost_ack");
	});

	test("a capture during an unresolved resize issues no keeper history request", async () => {
		const harness = await makeHarness();
		await enableStream(harness.manager, STREAM_A);
		startTerminalRecording(START_COMMAND, { sessionMgr: harness.manager });
		harness.manager.appendScrollback(CHANNEL_ID, Buffer.from("before-resize\r\n"));

		const pool = getMultiplexedPool();
		const priorGetHistory = pool.getHistoryRecords.bind(pool);
		let historyCalls = 0;
		pool.getHistoryRecords = async (channelId: number) => {
			historyCalls += 1;
			return priorGetHistory(channelId);
		};
		try {
			const state = harness.manager.terminalStreams.get(CHANNEL_ID)!;
			// Open the gate and leave it unresolved: the core is frozen, captured
			// bytes are retained, and no boundary has been proven.
			installLiveResizeCapture(
				harness.manager, CHANNEL_ID, state, 7, TEST_COLS, TEST_ROWS, 10, 3,
			);
			expect(state.resizeCapture?.boundaryApplied).toBe(false);

			const result = await captureTerminalIncident(
				captureCommandFixture({ session_id: String(SESSION_ID) }),
				{ sessionMgr: harness.manager },
			);
			expect(result.status).toBe("partial");
			expect(historyCalls).toBe(0);
			const bundle = readBundle(result.path!);
			// The frozen core still answered its own bounded reads.
			expect(bundle.worker.geometry).toEqual({ cols: TEST_COLS, rows: TEST_ROWS });
			expect(bundle.worker.resizes[0]!.boundary_offset).toBeNull();
		} finally {
			pool.getHistoryRecords = priorGetHistory;
		}
	});
});

function unresolvedCapture(installSeq: number): LiveResizeCapture {
	return {
		streamId: STREAM_A,
		resizeSeq: 11,
		installSeq,
		fromCols: TEST_COLS,
		fromRows: TEST_ROWS,
		toCols: 10,
		toRows: 3,
		queryCarry: new Uint8Array(0),
		capturedBytes: 24,
		capturedChunks: 2,
		boundarySeq: -1,
		boundaryApplied: false,
		failedReason: null,
	};
}

interface ResizeBundleShape {
	readonly coverage: {
		readonly core_replay: string;
		readonly core_replay_reasons: readonly string[];
	};
	readonly worker: {
		readonly geometry: { readonly cols: number; readonly rows: number } | null;
		readonly resizes: readonly {
			readonly outcome: string;
			readonly boundary_offset: string | null;
		}[];
	};
}

function readBundle(path: string): ResizeBundleShape {
	return JSON.parse(gunzipSync(readFileSync(path)).toString("utf8")) as ResizeBundleShape;
}
