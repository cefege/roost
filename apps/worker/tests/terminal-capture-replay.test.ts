// End-to-end proof that a bundle this worker's writer produces actually
// replays: the recorder is armed BEFORE the fresh core's first byte, the real
// worker data path retains every chunk, a keeper-acknowledged resize lands a
// real boundary offset, and scripts/replay-terminal-incident-layers.ts rebuilds
// the same screen from the retained bytes alone.
//
// It also pins the two honest refusals: a recording armed mid-session and a
// recording whose raw prefix was evicted must report missing coverage, never a
// core defect. The layers module is imported by relative path because scripts/
// sits outside the bun workspace.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { gunzipSync } from "node:zlib";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { spansText } from "@roost/shared/cell";
import {
	TERMINAL_CAPTURE_LIMITS,
	type TerminalIncidentBundle,
} from "@roost/shared/terminal-capture";
import {
	matchLayers,
	replayRawToCore,
	workerCoreCheckpoints,
	workerFoldCheckpoints,
	type CheckpointSet,
	type LayerCheckpoint,
} from "../../../scripts/replay-terminal-incident-layers.ts";
import { _resetCaptureStorageForTest } from "../src/diag/capture-storage.ts";
import {
	captureTerminalIncident,
	startTerminalRecording,
	_resetTerminalCaptureForTest,
	_terminalRecorderForTest,
} from "../src/diag/terminal-capture.ts";
import { installAutoKeeper } from "./keeper-fake-pool.ts";
import {
	cleanupStreamHarnesses,
	enableStream,
	flushLeadingCellEmit,
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
	capture_id: "eeeeeeee-0000-4000-8000-00000000ee06",
	reason: "manual",
	browser_evidence_json: "",
} as const;

/** The photographed defect's shape: an elapsed-time footer rewritten in place
 *  on the bottom row, with no newline between the two versions. */
const OLD_FOOTER = "FOOTER-12s";
const NEW_FOOTER = "FOOTER-14s";
const SHRUNK_ROWS = 4;

let captureRoot: string;
const priorLogDir = process.env.ROOST_WORKER_LOG_DIR;

beforeEach(() => {
	_resetTerminalCaptureForTest();
	_resetCaptureStorageForTest();
	captureRoot = mkdtempSync(join(tmpdir(), "roost-capture-replay-"));
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

describe("worker terminal capture replay", () => {
	test("a recording armed at the first byte replays to one overwritten footer", async () => {
		trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
		const harness = await makeHarness();
		await enableStream(harness.manager, STREAM_A);
		await flushLeadingCellEmit();
		// Armed BEFORE the fresh core has parsed a single byte: this is the only
		// state from which exact parser replay is possible at all.
		expect(harness.record.head_seq).toBe(0);
		expect(startTerminalRecording(START_COMMAND, { sessionMgr: harness.manager }).status)
			.toBe("recording");

		harness.manager.appendScrollback(
			CHANNEL_ID,
			Buffer.from(`\x1b[${TEST_ROWS};1H${OLD_FOOTER}`),
		);
		harness.manager.emitCellFrame(CHANNEL_ID, true);
		const recorder = _terminalRecorderForTest(String(SESSION_ID))!;
		// One simulated second later the application rewrites the SAME row: bare
		// CR, no newline, so nothing scrolls.
		recorder.last_sample_mono_ms = Number.NEGATIVE_INFINITY;
		harness.manager.appendScrollback(CHANNEL_ID, Buffer.from(`\r${NEW_FOOTER}`));
		harness.manager.emitCellFrame(CHANNEL_ID, false);

		const resized = await enableStream(harness.manager, STREAM_B, TEST_COLS, SHRUNK_ROWS);
		expect(resized).toMatchObject({ status: "committed", resized: true });
		const boundaryHeadSeq = harness.record.head_seq;

		const result = await captureTerminalIncident(
			captureCommandFixture({ session_id: String(SESSION_ID) }),
			{ sessionMgr: harness.manager },
		);
		expect(result.error).toBeNull();
		const section = readBundle(result.path!).worker!;

		// The retained chunks are an unbroken offset chain from the segment's own
		// open offset, which is what makes the replay below admissible.
		const segment = section.segments[0]!;
		expect(segment.open_offset).toBe("0");
		expect(section.raw.map((record) => [record.start_offset, record.end_offset]))
			.toEqual([["0", "16"], ["16", "27"]]);
		expect(section.raw.every((record) => record.segment_id === segment.segment_id)).toBe(true);

		// The resize boundary is the keeper-acknowledged raw offset, never a
		// request timestamp.
		const resize = section.resizes.find((record) => record.outcome === "accepted")!;
		expect(resize.boundary_offset).toBe(String(boundaryHeadSeq));
		expect(resize.to).toEqual({ cols: TEST_COLS, rows: SHRUNK_ROWS });

		const folds = workerFoldCheckpoints(section);
		const replay = await replayRawToCore(section, folds);
		expect(replay).toMatchObject({
			status: "complete",
			reason: "complete",
			difference: null,
			comparedAgainst: "worker_fold",
			resizes: 1,
		});
		expect(replay.bytes).toBe(27);

		// The screen the replay proved equal carries the new footer once and the
		// older one not at all — one footer, not two.
		expect(footerCounts(folds)).toEqual({ old: 0, new: 1 });

		// Sampled core scans agree with the shipped fold wherever both exist; the
		// unsampled interval is unmatched coverage, never a difference.
		const match = matchLayers(workerCoreCheckpoints(section), folds);
		expect(match.difference).toBeNull();
		expect(match.matched).toBeGreaterThan(0);
		expect(match.unmatched).toBeGreaterThan(0);
	});

	test("a recording armed after the first byte refuses to claim exact replay", async () => {
		const harness = await makeHarness();
		await enableStream(harness.manager, STREAM_A);
		await flushLeadingCellEmit();
		// The real production situation: the shell already initialized itself.
		harness.manager.appendScrollback(CHANNEL_ID, Buffer.from("\x1b[1;1Halready-here"));
		expect(harness.record.head_seq).toBeGreaterThan(0);

		expect(startTerminalRecording(START_COMMAND, { sessionMgr: harness.manager }).status)
			.toBe("recording");
		harness.manager.appendScrollback(
			CHANNEL_ID,
			Buffer.from(`\x1b[${TEST_ROWS};1H${NEW_FOOTER}`),
		);
		harness.manager.emitCellFrame(CHANNEL_ID, true);

		const result = await captureTerminalIncident(
			captureCommandFixture({ session_id: String(SESSION_ID) }),
			{ sessionMgr: harness.manager },
		);
		const bundle = readBundle(result.path!);
		expect(bundle.coverage.core_replay).toBe("partial");
		expect(bundle.coverage.core_replay_reasons).toContain("missing_initial_prefix");

		const section = bundle.worker!;
		expect(section.segments[0]!.open_offset).not.toBe("0");
		const replay = await replayRawToCore(section, workerFoldCheckpoints(section));
		expect(replay.status).toBe("unavailable");
		expect(replay.reason).toBe("missing_initial_prefix");
		// Nothing was compared, so nothing may be concluded about the core.
		expect(replay.difference).toBeNull();
		expect(replay.comparedAgainst).toBe("none");
	});

	test("an evicted raw prefix refuses to claim exact replay", async () => {
		const harness = await makeHarness();
		await enableStream(harness.manager, STREAM_A);
		await flushLeadingCellEmit();
		expect(startTerminalRecording(START_COMMAND, { sessionMgr: harness.manager }).status)
			.toBe("recording");

		// Push past the retained-record bound so the pools drop the oldest raw
		// records from the FRONT — the prefix the parser state depends on.
		const overflow = TERMINAL_CAPTURE_LIMITS.layerEntries + 4;
		for (let idx = 0; idx < overflow; idx += 1) {
			harness.manager.appendScrollback(CHANNEL_ID, Buffer.from(`\x1b[1;1Hrow${idx}`));
		}
		harness.manager.emitCellFrame(CHANNEL_ID, true);
		const recorder = _terminalRecorderForTest(String(SESSION_ID))!;
		expect(recorder.raw_prefix_complete).toBe(false);

		const result = await captureTerminalIncident(
			captureCommandFixture({ session_id: String(SESSION_ID) }),
			{ sessionMgr: harness.manager },
		);
		const bundle = readBundle(result.path!);
		const section = bundle.worker!;
		// The bundle NAMES the evicted range rather than presenting a short chain
		// as a complete one.
		const omission = section.omissions.find((entry) => entry.name === "worker.raw")!;
		expect(omission.reason).toBe("raw_prefix_evicted");
		expect(omission.dropped_count).toBe(4);
		expect(bundle.coverage.core_replay_reasons).toContain("raw_prefix_evicted");

		const replay = await replayRawToCore(section, workerFoldCheckpoints(section));
		expect(replay.status).toBe("unavailable");
		expect(replay.reason).toBe("raw_prefix_evicted");
		expect(replay.difference).toBeNull();
		expect(replay.comparedAgainst).toBe("none");
	});
});

/** How many viewport rows of the FINAL proven screen carry each footer. The
 *  replay proved this screen equal to the bytes, so counting here counts the
 *  replayed grid. */
function footerCounts(folds: CheckpointSet): { old: number; new: number } {
	let last: LayerCheckpoint | null = null;
	for (const checkpoint of folds.checkpoints.values()) last = checkpoint;
	let oldCount = 0;
	let newCount = 0;
	for (const row of last?.view.viewportRows ?? []) {
		const text = spansText(row.spans);
		if (text.includes(OLD_FOOTER)) oldCount += 1;
		if (text.includes(NEW_FOOTER)) newCount += 1;
	}
	return { old: oldCount, new: newCount };
}

function readBundle(path: string): TerminalIncidentBundle {
	return JSON.parse(gunzipSync(readFileSync(path)).toString("utf8")) as TerminalIncidentBundle;
}
