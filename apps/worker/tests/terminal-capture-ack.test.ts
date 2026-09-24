// The `recent_worker_capture` member of every worker capture ack. Verify:
//   - a requested CAPTURE reports its own saved file in path/byte_length and
//     never echoes that same capture back as a worker-detected incident
//   - only a worker-local emission conflict populates recent_worker_capture,
//     and it then surfaces on the next lease, capture and stop acks
//   - a peer-authored trigger claiming worker origin cannot buy that claim
// A false "the worker detected an incident" notice for a capture the operator
// requested is exactly the attribution error this tooling exists to avoid.

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
} from "@roost/protocol/cell";
import { isTerminalCaptureFileName } from "@roost/protocol/terminal-capture";
import { _resetCaptureStorageForTest } from "../src/diag/capture-storage.ts";
import {
	captureTerminalIncident,
	noteAcceptedCellEmission,
	startTerminalRecording,
	stopTerminalRecording,
	_resetTerminalCaptureForTest,
	_settleScheduledCaptures,
} from "../src/diag/terminal-capture.ts";
import type { SessionManager } from "../src/session-manager.ts";
import type { SessionShellRecord } from "../src/session-record.ts";
import {
	cleanupStreamHarnesses,
	enableStream,
	makeHarness,
	paintRows,
	SESSION_ID,
	CHANNEL_ID,
	STREAM_A,
	TEST_ROWS,
} from "./terminal-stream-state-harness.ts";
import {
	browserPayload,
	captureCommandFixture,
	FIXTURE_RECORDING_ID,
} from "./terminal-capture-fixtures.ts";

const MANUAL_CAPTURE_ID = "eeeeeeee-0000-4000-8000-00000000ea01";
const SECOND_CAPTURE_ID = "eeeeeeee-0000-4000-8000-00000000ea02";
const LEASE_CAPTURE_ID = "eeeeeeee-0000-4000-8000-00000000ea03";
const STOP_CAPTURE_ID = "eeeeeeee-0000-4000-8000-00000000ea04";
const SPOOF_CAPTURE_ID = "eeeeeeee-0000-4000-8000-00000000ea05";

const START_COMMAND = {
	action: "start",
	session_id: String(SESSION_ID),
	recording_id: FIXTURE_RECORDING_ID,
	capture_id: LEASE_CAPTURE_ID,
	reason: "manual",
	browser_evidence_json: "",
} as const;

let captureRoot: string;
let captureDir: string;
const priorLogDir = process.env.ROOST_WORKER_LOG_DIR;

beforeEach(() => {
	_resetTerminalCaptureForTest();
	_resetCaptureStorageForTest();
	captureRoot = mkdtempSync(join(tmpdir(), "roost-capture-ack-"));
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

describe("worker capture ack attribution", () => {
	test("a requested capture reports its own file, never itself as an incident", async () => {
		const manager = await armedManager();
		const command = captureCommandFixture({ capture_id: MANUAL_CAPTURE_ID });

		const ack = await captureTerminalIncident(command, { sessionMgr: manager });
		expect(ack.error).toBeNull();
		expect(ack.path).not.toBeNull();
		expect(ack.byte_length).toBeGreaterThan(0);
		expect(ack.recent_worker_capture).toBeNull();

		// The retry replays that same result, so the false attribution cannot
		// reappear through the idempotency cache either.
		const retry = await captureTerminalIncident(command, { sessionMgr: manager });
		expect(retry.path).toBe(ack.path);
		expect(retry.recent_worker_capture).toBeNull();
		expect(capturedFiles()).toHaveLength(1);
	});

	test("a worker-local emission conflict surfaces on the next acks", async () => {
		const manager = await armedManager();
		const record = shellRecordOf(manager);
		const requested = await captureTerminalIncident(
			captureCommandFixture({ capture_id: MANUAL_CAPTURE_ID }),
			{ sessionMgr: manager },
		);
		expect(requested.recent_worker_capture).toBeNull();

		injectCoreFoldMismatch(manager, record);
		await _settleScheduledCaptures();
		const workerFile = capturedFiles().find((name) => !name.includes(MANUAL_CAPTURE_ID));
		expect(workerFile).toBeDefined();

		// The browser never asked for that one, so the lease ack is how it
		// learns the file exists.
		const renewed = startTerminalRecording(START_COMMAND, { sessionMgr: manager });
		expect(renewed.status).toBe("recording");
		const incident = renewed.recent_worker_capture!;
		expect(incident.capture_id).not.toBe(MANUAL_CAPTURE_ID);
		expect(incident.path.endsWith(workerFile!)).toBe(true);
		expect(readTrigger(incident.path).reason).toBe("worker_emission");

		// A later requested capture still reports its OWN file separately.
		const second = await captureTerminalIncident(
			captureCommandFixture({ capture_id: SECOND_CAPTURE_ID, reason: "pre_repair" }),
			{ sessionMgr: manager },
		);
		expect(second.path).not.toBe(incident.path);
		expect(second.recent_worker_capture).toEqual(incident);

		const stopped = stopTerminalRecording({
			...START_COMMAND,
			action: "stop",
			capture_id: STOP_CAPTURE_ID,
		});
		expect(stopped.status).toBe("stopped");
		expect(stopped.recent_worker_capture).toEqual(incident);
	});

	test("a peer-authored worker trigger cannot claim a worker detection", async () => {
		const manager = await armedManager();
		const payload = browserPayload(SPOOF_CAPTURE_ID, { dom_history: [], gaps: [] });
		const spoofed = {
			...payload,
			trigger: {
				...payload.trigger,
				reason: "worker_emission",
				origin: "worker",
				detail: "core_fold_disagreement",
			},
		};

		const ack = await captureTerminalIncident(
			captureCommandFixture({
				capture_id: SPOOF_CAPTURE_ID,
				browser_evidence_json: JSON.stringify(spoofed),
			}),
			{ sessionMgr: manager },
		);
		expect(ack.error).toBeNull();
		expect(ack.recent_worker_capture).toBeNull();
		// The authoring layer is a structural fact: the saved bundle records the
		// browser as the origin, so a replay report cannot attribute this to the
		// worker either.
		expect(readTrigger(ack.path!).origin).toBe("browser");

		const renewed = startTerminalRecording(START_COMMAND, { sessionMgr: manager });
		expect(renewed.recent_worker_capture).toBeNull();
	});
});

async function armedManager(): Promise<SessionManager> {
	const harness = await makeHarness();
	await enableStream(harness.manager, STREAM_A);
	paintRows(harness.record.wtermCore, ["FOOTER-12s", "row-1", "row-2"]);
	expect(startTerminalRecording(START_COMMAND, { sessionMgr: harness.manager }).status)
		.toBe("recording");
	return harness.manager;
}

function shellRecordOf(manager: SessionManager): SessionShellRecord {
	return manager.getBySessionId(String(SESSION_ID)) as SessionShellRecord;
}

/** Emit a full whose top row disagrees with the core: the shape of "the grid
 *  the worker shipped is not the grid the core holds", which is the only
 *  trigger that may name a capture as worker-detected. */
function injectCoreFoldMismatch(manager: SessionManager, record: SessionShellRecord): void {
	const core = record.wtermCore;
	const full: CellGridFrame = gridToCellFrame(
		core,
		1,
		cellGridEpoch(record.cell_emit),
		STREAM_A,
		0,
		scrollbackOrigin(core, record.cell_emit),
	);
	noteAcceptedCellEmission(record, manager.terminalStreams.get(CHANNEL_ID)!, {
		...full,
		viewportRows: [
			{
				index: 0,
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

function readTrigger(path: string): { readonly reason: string; readonly origin: string } {
	const bundle = JSON.parse(gunzipSync(readFileSync(path)).toString("utf8")) as {
		readonly trigger: { readonly reason: string; readonly origin: string };
	};
	return bundle.trigger;
}
