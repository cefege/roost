// Remote-evidence nesting for one CAPTURE. Verify:
//   - a correctly nested browser payload lands as the bundle's `browser`
//     section, carrying its own captured_at_ms, and its authored trigger
//     survives instead of being replaced by a synthesized one
//   - a correctly nested coordinator payload lands the same way
//   - a nested-but-invalid section is dropped by the write gate WITHOUT costing
//     the worker its own evidence
//   - a FLATTENED payload — the production defect where a layer's fields sat on
//     the envelope with no member named for the layer — is refused outright
//     rather than accepted and silently discarded
// A layer that shipped evidence must never end up as a null section with only
// an omission to show for it; that is the defect this file exists to pin.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { gunzipSync } from "node:zlib";
import { join } from "node:path";
import { tmpdir } from "node:os";
import type {
	TerminalIncidentBundle,
	TerminalWorkerSection,
} from "@roost/protocol/terminal-capture";
import type { SessionManager } from "../src/session-manager.ts";
import { _resetCaptureStorageForTest } from "../src/diag/capture-storage.ts";
import {
	captureTerminalIncident,
	startTerminalRecording,
	_resetTerminalCaptureForTest,
} from "../src/diag/terminal-capture.ts";
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
	coordinatorPayload,
	flattenedBrowserPayload,
	invalidTriggerPayload,
	malformedSectionPayload,
	FIXTURE_AT_MS,
	FIXTURE_RECORDING_ID,
} from "./terminal-capture-fixtures.ts";

const START_COMMAND = {
	action: "start",
	session_id: String(SESSION_ID),
	recording_id: FIXTURE_RECORDING_ID,
	capture_id: "eeeeeeee-0000-4000-8000-00000000ee07",
	reason: "manual",
	browser_evidence_json: "",
} as const;

let captureRoot: string;
const priorLogDir = process.env.ROOST_WORKER_LOG_DIR;

beforeEach(() => {
	_resetTerminalCaptureForTest();
	_resetCaptureStorageForTest();
	captureRoot = mkdtempSync(join(tmpdir(), "roost-capture-evidence-"));
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

describe("worker terminal capture remote evidence", () => {
	test("both nested remote payloads land as their own bundle sections", async () => {
		const harness = await armed();
		const command = captureCommandFixture({ session_id: String(SESSION_ID) });
		const result = await captureTerminalIncident(
			{
				...command,
				browser_evidence_json: JSON.stringify(
					browserPayload(command.capture_id, { dom_history: [], gaps: [] }),
				),
				coordinator_evidence_json: JSON.stringify(coordinatorPayload(command.capture_id)),
			},
			{ sessionMgr: harness.manager },
		);
		expect(result.error).toBeNull();
		const bundle = readBundle(result.path!);

		expect(bundle.browser).not.toBeNull();
		expect(bundle.browser!.layer).toBe("browser");
		expect(bundle.browser!.captured_at_ms).toBe(FIXTURE_AT_MS);
		expect(bundle.coordinator).not.toBeNull();
		expect(bundle.coordinator!.layer).toBe("coordinator");
		expect(bundle.coordinator!.captured_at_ms).toBe(FIXTURE_AT_MS);
		expect(workerSectionOf(bundle).omissions.some((entry) => entry.name.startsWith("remote:")))
			.toBe(false);

		// The browser authored the trigger: it is the only layer that knows which
		// invariant fired and how many occurrences the latch collapsed.
		expect(bundle.trigger).toMatchObject({
			reason: "history_identity",
			origin: "browser",
			detail: "duplicate_history_index",
			occurrence_count: 3,
		});
	});

	test("a nested but invalid section is dropped without costing worker evidence", async () => {
		const harness = await armed();
		harness.manager.appendScrollback(CHANNEL_ID, Buffer.from("worker-kept\r\n"));

		const command = captureCommandFixture({ session_id: String(SESSION_ID) });
		const result = await captureTerminalIncident(
			{
				...command,
				browser_evidence_json: JSON.stringify(malformedSectionPayload(command.capture_id)),
			},
			{ sessionMgr: harness.manager },
		);
		// Envelope and nesting were fine, so the capture still succeeds; only the
		// layer that failed the write-side bundle gate is absent.
		expect(result.error).toBeNull();
		expect(result.status).toBe("partial");
		const bundle = readBundle(result.path!);
		expect(bundle.browser).toBeNull();
		const dropped = workerSectionOf(bundle).omissions.find(
			(entry) => entry.name.startsWith("remote:"),
		)!;
		expect(dropped.name).toBe("remote:browser.events");
		expect(dropped.reason).toBe("layer_unavailable");
		expect(workerSectionOf(bundle).raw.length).toBeGreaterThan(0);
	});

	test("a flattened payload with no nested layer member is refused outright", async () => {
		const harness = await armed();
		const command = captureCommandFixture({ session_id: String(SESSION_ID) });
		const result = await captureTerminalIncident(
			{
				...command,
				browser_evidence_json: JSON.stringify(flattenedBrowserPayload(command.capture_id)),
			},
			{ sessionMgr: harness.manager },
		);
		// Refused, not accepted-and-discarded: a layer shipping the wrong shape
		// must learn about it rather than lose its evidence to an omission line.
		expect(result.status).toBe("error");
		expect(result.error).toBe("evidence_malformed");
		expect(result.path).toBeNull();
	});

	test("an invalid peer-authored trigger is dropped, not paid for in evidence", async () => {
		const harness = await armed();
		harness.manager.appendScrollback(CHANNEL_ID, Buffer.from("worker-kept\r\n"));

		const command = captureCommandFixture({ session_id: String(SESSION_ID) });
		const result = await captureTerminalIncident(
			{
				...command,
				browser_evidence_json: JSON.stringify(invalidTriggerPayload(command.capture_id)),
			},
			{ sessionMgr: harness.manager },
		);
		// The trigger is adopted from the peer, so an invalid one would otherwise
		// fail the write gate AND its retry, losing the worker's whole section.
		expect(result.error).toBeNull();
		expect(result.status).toBe("partial");
		const bundle = readBundle(result.path!);
		expect(bundle.trigger).toMatchObject({
			reason: "manual",
			origin: "browser",
			occurrence_count: 0,
		});
		expect(bundle.browser).toBeNull();
		expect(workerSectionOf(bundle).raw.length).toBeGreaterThan(0);
		expect(workerSectionOf(bundle).omissions.some((entry) => entry.name.startsWith("remote:")))
			.toBe(true);
	});
});

interface EvidenceHarness {
	readonly manager: SessionManager;
}

async function armed(): Promise<EvidenceHarness> {
	const harness = await makeHarness();
	await enableStream(harness.manager, STREAM_A);
	expect(startTerminalRecording(START_COMMAND, { sessionMgr: harness.manager }).status)
		.toBe("recording");
	return { manager: harness.manager };
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
