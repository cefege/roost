// Worker terminal-incident recorder behavior. Verify:
//   - repeated same-generation mismatch writes ONE file; a new sequence alone
//     does not buy another, and neither does a new grid epoch (the 60s
//     session-wide automatic floor is not per-identity)
//   - a healthy full+delta sequence stays silent
//   - a dropped delta invalidates the fold until the next accepted full
//   - lease expiry and session close free every owned record
//   - cap eviction makes replay explicitly partial with a named reason
// Mismatch is injected at the tap boundary (the accepted-emission observer),
// which is the only place a real core/fold disagreement would be observed.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { gunzipSync } from "node:zlib";
import { join } from "node:path";
import { tmpdir } from "node:os";
import {
	cellGridEpoch,
	gridToCellFrame,
	scrollbackOrigin,
	type CellGridFrame,
} from "@roost/shared/cell";
import {
	isTerminalCaptureFileName,
	TERMINAL_CAPTURE_LIMITS,
} from "@roost/shared/terminal-capture";
import { _resetCaptureStorageForTest } from "../src/diag/capture-storage.ts";
import {
	captureTerminalIncident,
	noteAcceptedCellEmission,
	noteRejectedCellEmission,
	noteRetainedRawChunk,
	startTerminalRecording,
	stopTerminalRecording,
	terminalRecorderArmed,
	_resetTerminalCaptureForTest,
	_settleScheduledCaptures,
	_terminalRecorderForTest,
} from "../src/diag/terminal-capture.ts";
import type { SessionManager } from "../src/session-manager.ts";
import type { SessionShellRecord } from "../src/session-record.ts";
import type { TerminalStreamState } from "../src/session-terminal-state.ts";
import {
	cleanupStreamHarnesses,
	enableStream,
	makeHarness,
	paintRows,
	SESSION_ID,
	CHANNEL_ID,
	STREAM_A,
	TEST_COLS,
	TEST_ROWS,
} from "./terminal-stream-state-harness.ts";
import { captureCommandFixture, FIXTURE_RECORDING_ID } from "./terminal-capture-fixtures.ts";

const START_COMMAND = {
	action: "start",
	session_id: String(SESSION_ID),
	recording_id: FIXTURE_RECORDING_ID,
	capture_id: "eeeeeeee-0000-4000-8000-00000000ee01",
	reason: "manual",
	browser_evidence_json: "",
} as const;

const RIVAL_RECORDING_ID = "cccccccc-0000-4000-8000-00000000cc99";

let captureRoot: string;
let captureDir: string;
const priorLogDir = process.env.ROOST_WORKER_LOG_DIR;

beforeEach(() => {
	_resetTerminalCaptureForTest();
	_resetCaptureStorageForTest();
	captureRoot = mkdtempSync(join(tmpdir(), "roost-capture-"));
	captureDir = join(captureRoot, "RoostWorker");
	process.env.ROOST_WORKER_LOG_DIR = captureDir;
});

afterEach(() => {
	cleanupStreamHarnesses();
	_resetTerminalCaptureForTest();
	_resetCaptureStorageForTest();
	if (priorLogDir === undefined) delete process.env.ROOST_WORKER_LOG_DIR;
	else process.env.ROOST_WORKER_LOG_DIR = priorLogDir;
	try { rmSync(captureRoot, { recursive: true, force: true }); } catch { /* ignore */ }
});

describe("worker terminal capture recorder", () => {
	test("a repeating same-generation mismatch writes exactly one file", async () => {
		const harness = await armedHarness();
		const state = streamStateOf(harness.manager);

		tapMismatch(harness.record, state, 1);
		await _settleScheduledCaptures();
		expect(capturedFiles()).toHaveLength(1);

		// Isolate the LATCH: clear the session-wide floor and the sample interval
		// so nothing but the per-identity latch can suppress the second capture.
		const recorder = _terminalRecorderForTest(String(SESSION_ID))!;
		recorder.last_automatic_ms = Number.NEGATIVE_INFINITY;
		recorder.last_sample_mono_ms = Number.NEGATIVE_INFINITY;
		tapMismatch(harness.record, state, 2);
		await _settleScheduledCaptures();
		expect(capturedFiles()).toHaveLength(1);
		expect(recorder.occurrences.get([...recorder.latches][0]!)).toBe(2);
	});

	test("a new grid epoch does not bypass the session-wide automatic floor", async () => {
		const harness = await armedHarness();
		const state = streamStateOf(harness.manager);

		tapMismatch(harness.record, state, 1);
		await _settleScheduledCaptures();
		expect(capturedFiles()).toHaveLength(1);

		// A fresh epoch is a fresh latch key, so only the 60s floor can stop it.
		const recorder = _terminalRecorderForTest(String(SESSION_ID))!;
		recorder.last_sample_mono_ms = Number.NEGATIVE_INFINITY;
		harness.record.cell_emit = {
			...harness.record.cell_emit,
			gridEpochRevision: harness.record.cell_emit.gridEpochRevision + 1,
		};
		tapMismatch(harness.record, state, 2);
		await _settleScheduledCaptures();
		expect(capturedFiles()).toHaveLength(1);
		expect(recorder.latches.size).toBe(1);
	});

	test("a healthy full then delta stays silent and is sampled both times", async () => {
		const harness = await armedHarness();
		const state = streamStateOf(harness.manager);
		const recorder = _terminalRecorderForTest(String(SESSION_ID))!;

		noteAcceptedCellEmission(harness.record, state, liveFullFrame(harness.record, 1));
		// Clear BOTH sampling gates: the interval, and the budget suppression a
		// slow first scan on a loaded machine would otherwise leave armed.
		recorder.last_sample_mono_ms = Number.NEGATIVE_INFINITY;
		recorder.sample_suppressed_until_mono_ms = Number.NEGATIVE_INFINITY;
		harness.record.wtermCore.writeString("\x1b[3;1Hnext");
		noteAcceptedCellEmission(harness.record, state, liveFullFrame(harness.record, 2));

		await _settleScheduledCaptures();
		expect(capturedFiles()).toHaveLength(0);
		expect(recorder.sampling.sampled).toBe(2);
		expect(recorder.emissions.map((entry) => entry.record.comparison)).toEqual([
			"equal",
			"equal",
		]);
	});

	test("a dropped delta invalidates the fold until the next accepted full", async () => {
		const harness = await armedHarness();
		const state = streamStateOf(harness.manager);
		const recorder = _terminalRecorderForTest(String(SESSION_ID))!;

		noteAcceptedCellEmission(harness.record, state, liveFullFrame(harness.record, 1));
		expect(recorder.fold).not.toBeNull();

		noteRejectedCellEmission(harness.record, "baseline_invalidated");
		expect(recorder.fold).toBeNull();
		expect([...recorder.fold_reasons]).toContain("baseline_invalidated");

		// A sparse delta cannot rebuild a baseline it no longer has.
		noteAcceptedCellEmission(harness.record, state, deltaFrameFrom(harness.record, 2));
		expect(recorder.fold).toBeNull();
		expect(recorder.emissions.at(-1)!.record.comparison).toBe("baseline_invalid");

		noteAcceptedCellEmission(harness.record, state, liveFullFrame(harness.record, 3));
		expect(recorder.fold).not.toBeNull();
	});

	test("lease expiry disarms the recorder and frees its records", async () => {
		const harness = await armedHarness();
		const state = streamStateOf(harness.manager);
		noteAcceptedCellEmission(harness.record, state, liveFullFrame(harness.record, 1));
		const recorder = _terminalRecorderForTest(String(SESSION_ID))!;
		expect(recorder.emissions.length).toBe(1);

		recorder.expires_at_ms = Date.now() - 1;
		expect(terminalRecorderArmed(String(SESSION_ID))).toBe(false);
		expect(_terminalRecorderForTest(String(SESSION_ID))).toBeUndefined();

		// A later tap on the same session allocates nothing.
		noteRetainedRawChunk(String(SESSION_ID), 8, new Uint8Array(8));
		expect(_terminalRecorderForTest(String(SESSION_ID))).toBeUndefined();
	});

	test("session close frees the recorder", async () => {
		const harness = await armedHarness();
		expect(_terminalRecorderForTest(String(SESSION_ID))).toBeDefined();
		harness.manager._dropChannelState(CHANNEL_ID);
		expect(_terminalRecorderForTest(String(SESSION_ID))).toBeUndefined();
	});

	test("a second recording on a live lease conflicts instead of evicting it", async () => {
		const harness = await armedHarness();
		const conflict = startTerminalRecording(
			{ ...START_COMMAND, recording_id: RIVAL_RECORDING_ID },
			{ sessionMgr: harness.manager },
		);
		expect(conflict).toMatchObject({ status: "error", error: "lease_conflict" });
		expect(_terminalRecorderForTest(String(SESSION_ID))!.recording_id)
			.toBe(FIXTURE_RECORDING_ID);

		// STOP from a different recording is denied; the owner's STOP is not, and
		// a repeat by the owner is harmless.
		expect(stopTerminalRecording({
			...START_COMMAND,
			action: "stop",
			recording_id: RIVAL_RECORDING_ID,
		})).toMatchObject({ status: "error", error: "permission_denied" });
		expect(stopTerminalRecording({ ...START_COMMAND, action: "stop" }).status).toBe("stopped");
		expect(stopTerminalRecording({ ...START_COMMAND, action: "stop" }).status).toBe("stopped");
	});

	test("a renewing START preserves retained evidence", async () => {
		const harness = await armedHarness();
		const state = streamStateOf(harness.manager);
		noteAcceptedCellEmission(harness.record, state, liveFullFrame(harness.record, 1));
		const before = _terminalRecorderForTest(String(SESSION_ID))!;
		before.expires_at_ms = Date.now() + 1_000;

		const renewed = startTerminalRecording(START_COMMAND, { sessionMgr: harness.manager });
		expect(renewed.status).toBe("recording");
		expect(renewed.expires_at_ms).toBeGreaterThan(Date.now() + 60_000);
		expect(_terminalRecorderForTest(String(SESSION_ID))!.emissions.length).toBe(1);
	});

	test("raw cap eviction makes core replay explicitly partial with a named reason", async () => {
		const harness = await armedHarness();
		const chunk = new Uint8Array(16).fill(0x41);
		let endSeq = 0;
		for (let idx = 0; idx < TERMINAL_CAPTURE_LIMITS.layerEntries + 4; idx += 1) {
			endSeq += chunk.byteLength;
			noteRetainedRawChunk(String(SESSION_ID), endSeq, chunk);
		}
		const recorder = _terminalRecorderForTest(String(SESSION_ID))!;
		expect(recorder.raw.length).toBe(TERMINAL_CAPTURE_LIMITS.layerEntries);
		expect(recorder.raw_prefix_complete).toBe(false);

		const result = await captureTerminalIncident(
			captureCommandFixture({ session_id: String(SESSION_ID) }),
			{ sessionMgr: harness.manager },
		);
		expect(result.status).toBe("partial");
		const bundle = readBundle(result.path!);
		expect(bundle.coverage.core_replay).toBe("partial");
		expect(bundle.coverage.core_replay_reasons).toContain("raw_prefix_evicted");
		const omission = bundle.worker.omissions.find(
			(entry) => entry.name === "worker.raw",
		)!;
		expect(omission.reason).toBe("raw_prefix_evicted");
		expect(omission.dropped_count).toBe(4);
		expect(omission.range).toEqual({ start: "0", end: "64" });
	});

	test("an unarmed manual capture still carries the legacy raw tail", async () => {
		const harness = await makeHarness();
		await enableStream(harness.manager, STREAM_A);
		harness.manager.appendScrollback(CHANNEL_ID, Buffer.from("hello-unarmed"));

		const result = await captureTerminalIncident(
			captureCommandFixture({ session_id: String(SESSION_ID) }),
			{ sessionMgr: harness.manager },
		);
		expect(result.status).toBe("partial");
		const bundle = readBundle(result.path!);
		expect(bundle.worker.byte_capture!.byte_length).toBe("hello-unarmed".length);
		expect(bundle.coverage.core_replay).toBe("unavailable");
		expect(bundle.coverage.core_replay_reasons).toEqual(["layer_unavailable"]);
		expect(bundle.browser).toBeNull();
		expect(bundle.coordinator).toBeNull();
	});

	test("a retried capture ID returns the original result and writes no second file", async () => {
		const harness = await armedHarness();
		const command = captureCommandFixture({ session_id: String(SESSION_ID) });
		const first = await captureTerminalIncident(command, { sessionMgr: harness.manager });
		expect(first.path).not.toBeNull();
		const retry = await captureTerminalIncident(command, { sessionMgr: harness.manager });
		expect(retry).toEqual(first);
		expect(capturedFiles()).toHaveLength(1);
	});
});

interface ArmedHarness {
	readonly manager: SessionManager;
	readonly record: SessionShellRecord;
}

async function armedHarness(): Promise<ArmedHarness> {
	const harness = await makeHarness();
	await enableStream(harness.manager, STREAM_A);
	paintRows(harness.record.wtermCore, ["FOOTER-12s", "row-1", "row-2"]);
	const armed = startTerminalRecording(START_COMMAND, { sessionMgr: harness.manager });
	expect(armed.status).toBe("recording");
	return { manager: harness.manager, record: harness.record };
}

function streamStateOf(manager: SessionManager): TerminalStreamState {
	return manager.terminalStreams.get(CHANNEL_ID)!;
}

/** A dense viewport-only full exactly as the live emitter would produce it. */
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

function deltaFrameFrom(record: SessionShellRecord, seq: number): CellGridFrame {
	const full = liveFullFrame(record, seq);
	return {
		...full,
		full: false,
		baseSeq: seq - 1,
		viewportRows: [full.viewportRows[0]!],
		scrollbackRows: [],
		scrollbackAppend: [],
	};
}

/** Inject a full whose top row disagrees with the core: the exact shape of "the
 *  grid the worker shipped is not the grid the core holds". */
function tapMismatch(
	record: SessionShellRecord,
	state: TerminalStreamState,
	seq: number,
): void {
	const full = liveFullFrame(record, seq);
	noteAcceptedCellEmission(record, state, {
		...full,
		viewportRows: [
			{
				index: 0,
				// Same painted width as the core's row, different text: the exact
				// shape of the duplicated-footer class this path hunts.
				spans: [{ text: "FOOTER-14s", columns: 10, fg: 256, bg: 256, flags: 0 }],
			},
			...full.viewportRows.slice(1, TEST_ROWS),
		],
	});
}

function capturedFiles(): string[] {
	try {
		return readdirSync(captureDir).filter(isTerminalCaptureFileName);
	} catch { return []; }
}

interface BundleShape {
	readonly coverage: {
		readonly core_replay: string;
		readonly core_replay_reasons: readonly string[];
	};
	readonly worker: {
		readonly omissions: readonly {
			readonly name: string;
			readonly reason: string;
			readonly dropped_count: number;
			readonly range: { readonly start: string; readonly end: string } | null;
		}[];
		readonly byte_capture: { readonly byte_length: number } | null;
	};
	readonly browser: unknown;
	readonly coordinator: unknown;
}

function readBundle(path: string): BundleShape {
	return JSON.parse(gunzipSync(readFileSync(path)).toString("utf8")) as BundleShape;
}
