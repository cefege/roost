// Worker terminal-incident capture assembly. Verify:
//   - the browser evidence's named absolute history rows are read back with
//     their exact indices THROUGH the payload nesting, and rows below the
//     retained floor or past the current total are reported evicted/unavailable
//     rather than silently omitted
//   - the core-sampling grid bound is real: an over-budget grid is never
//     scanned and says so in coverage
// Remote-payload nesting itself is pinned by terminal-capture-evidence.test.ts.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { gunzipSync } from "node:zlib";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createWtermCore } from "@roost/wterm/wterm-core-factory";
import {
	cellGridEpoch,
	gridToCellFrame,
	scrollbackOrigin,
	type CellGridFrame,
} from "@roost/protocol/cell";
import {
	TERMINAL_CAPTURE_LIMITS,
	type TerminalIncidentBundle,
	type TerminalWorkerSection,
} from "@roost/protocol/terminal-capture";
import { _resetCaptureStorageForTest } from "../../src/diag/capture-storage.ts";
import {
	captureTerminalIncident,
	noteAcceptedCellEmission,
	startTerminalRecording,
	_resetTerminalCaptureForTest,
	_terminalRecorderForTest,
} from "../../src/diag/terminal-capture.ts";
import type { SessionShellRecord } from "../../src/session/session-record.ts";
import {
	cleanupStreamHarnesses,
	enableStream,
	makeHarness,
	SESSION_ID,
	CHANNEL_ID,
	STREAM_A,
} from "./terminal-stream-state-harness.ts";
import {
	browserPayload,
	captureCommandFixture,
	FIXTURE_RECORDING_ID,
} from "./terminal-capture-fixtures.ts";

const START_COMMAND = {
	action: "start",
	session_id: String(SESSION_ID),
	recording_id: FIXTURE_RECORDING_ID,
	capture_id: "eeeeeeee-0000-4000-8000-00000000ee05",
	reason: "manual",
	browser_evidence_json: "",
} as const;

let captureRoot: string;
const priorLogDir = process.env.ROOST_WORKER_LOG_DIR;

beforeEach(() => {
	_resetTerminalCaptureForTest();
	_resetCaptureStorageForTest();
	captureRoot = mkdtempSync(join(tmpdir(), "roost-capture-assembly-"));
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

describe("worker terminal capture assembly", () => {
	test("browser-named history rows are read back at their absolute indices", async () => {
		const harness = await makeHarness();
		await enableStream(harness.manager, STREAM_A);
		startTerminalRecording(START_COMMAND, { sessionMgr: harness.manager });
		for (let line = 0; line < 30; line += 1) {
			harness.manager.appendScrollback(CHANNEL_ID, Buffer.from(`L${line}\r\n`));
		}
		const total = scrollbackTotalOf(harness.record);
		expect(total).toBeGreaterThan(10);

		const command = captureCommandFixture({ session_id: String(SESSION_ID) });
		const result = await captureTerminalIncident(
			{
				...command,
				browser_evidence_json: JSON.stringify(browserPayload(command.capture_id, {
					dom_history: [{ index: 5 }, { index: 6 }, { index: 7 }],
					gaps: [{ start: String(total + 4), end: String(total + 9) }],
				})),
			},
			{ sessionMgr: harness.manager },
		);
		expect(result.error).toBeNull();
		const bundle = readBundle(result.path!);
		// Browser evidence WAS supplied, so the bundle must carry the browser
		// section — a null here is the production defect where the whole layer
		// was parsed, mis-placed and then thrown away.
		expect(bundle.browser).not.toBeNull();
		expect(bundle.browser!.captured_at_ms).toBe(1_700_000_000_000);
		expect(bundle.browser!.layer).toBe("browser");
		// The worker read back exactly the rows named INSIDE the nested section,
		// which only works if the range derivation reads through the envelope.
		expect(workerSectionOf(bundle).history_ranges).toEqual([
			{ start: "5", end: "8", status: "present", rows: 3 },
			{ start: String(total + 4), end: String(total + 9), status: "unavailable", rows: 0 },
		]);
		expect(workerSectionOf(bundle).history_rows.map((row) => row.index)).toEqual([5, 6, 7]);
		// The named rows carry their real painted text, which is the point.
		expect(workerSectionOf(bundle).history_rows[0]!.spans[0]!.text).toBe("L5");
		// The browser authored the trigger; the worker must not overwrite it with
		// a synthesized one that erases which invariant fired.
		expect(bundle.trigger).toMatchObject({
			reason: "history_identity",
			origin: "browser",
			detail: "duplicate_history_index",
			occurrence_count: 3,
		});
	});

	test("a range below the retained floor is reported evicted, not omitted", async () => {
		const harness = await makeHarness();
		await enableStream(harness.manager, STREAM_A);
		startTerminalRecording(START_COMMAND, { sessionMgr: harness.manager });
		harness.manager.appendScrollback(CHANNEL_ID, Buffer.from("only-line\r\n"));
		// An origin above zero is exactly the "the browser holds rows this core
		// no longer has" case.
		harness.record.cell_emit = { ...harness.record.cell_emit, sbOrigin: 40 };

		const command = captureCommandFixture({ session_id: String(SESSION_ID) });
		const result = await captureTerminalIncident(
			{
				...command,
				browser_evidence_json: JSON.stringify(browserPayload(command.capture_id, {
					dom_history: [{ index: 3 }],
					gaps: [],
				})),
			},
			{ sessionMgr: harness.manager },
		);
		const bundle = readBundle(result.path!);
		expect(workerSectionOf(bundle).history_ranges).toEqual([
			{ start: "3", end: "4", status: "evicted", rows: 0 },
		]);
		expect(workerSectionOf(bundle).history_rows).toHaveLength(0);
		expect(workerSectionOf(bundle).scrollback_origin).toBe("40");
	});

	test("an over-budget grid is never scanned and says so in coverage", async () => {
		const wideCore = await createWtermCore(200, 200);
		const harness = await makeHarness(wideCore);
		await enableStream(harness.manager, STREAM_A, 200, 200);
		startTerminalRecording(START_COMMAND, { sessionMgr: harness.manager });

		const state = harness.manager.terminalStreams.get(CHANNEL_ID)!;
		noteAcceptedCellEmission(harness.record, state, liveFullFrame(harness.record, 1));

		const recorder = _terminalRecorderForTest(String(SESSION_ID))!;
		expect(200 * 200).toBeGreaterThan(TERMINAL_CAPTURE_LIMITS.coreSampleMaxCells);
		expect(recorder.sampling.sampled).toBe(0);
		expect(recorder.sampling.skipped_grid).toBe(1);
		expect(recorder.emissions[0]!.record.comparison).toBe("budget_skipped");

		const result = await captureTerminalIncident(
			captureCommandFixture({ session_id: String(SESSION_ID) }),
			{ sessionMgr: harness.manager },
		);
		const bundle = readBundle(result.path!);
		expect(bundle.coverage.core_comparison).toBe("unavailable");
		expect(bundle.coverage.core_comparison_reasons).toEqual(["grid_budget_exceeded"]);
		expect(workerSectionOf(bundle).core_samples).toHaveLength(0);
	});
});

function scrollbackTotalOf(record: SessionShellRecord): number {
	const core = record.wtermCore;
	return scrollbackOrigin(core, record.cell_emit) + core.getScrollbackCount();
}

function liveFullFrame(record: SessionShellRecord, seq: number): CellGridFrame {
	const core = record.wtermCore;
	return gridToCellFrame(
		core,
		seq,
		cellGridEpoch(record.cell_emit),
		STREAM_A,
		0,
		scrollbackOrigin(core, record.cell_emit),
	);
}

function readBundle(path: string): TerminalIncidentBundle {
	return JSON.parse(gunzipSync(readFileSync(path)).toString("utf8")) as TerminalIncidentBundle;
}

/** The worker's own section must be present in EVERY bundle: the remote-drop
 *  fallback exists precisely so a peer's payload can never remove it. */
function workerSectionOf(bundle: TerminalIncidentBundle): TerminalWorkerSection {
	expect(bundle.worker).not.toBeNull();
	return bundle.worker as TerminalWorkerSection;
}
