// Shared fixtures for the worker terminal-incident capture tests: a minimal
// valid worker section and bundle input, the wire command shape, and the two
// REMOTE payloads as they actually cross the wire.
// Used by byte-capture, terminal-capture-recorder/-resize/-wire/-assembly/
// -evidence tests. Kept here rather than inline so a bundle- or payload-shape
// change breaks one place.
//
// The remote payloads are typed as TerminalCaptureBrowserPayload /
// TerminalCaptureCoordinatorPayload on purpose: a flattened fixture — the
// production defect where a layer's evidence validated as an envelope and then
// failed as a section — stops compiling here.

import {
	TERMINAL_INCIDENT_SCHEMA,
	type TerminalCaptureBrowserPayload,
	type TerminalCaptureCoordinatorPayload,
	type TerminalCaptureCoverageReport,
	type TerminalCaptureTrigger,
	type TerminalWorkerSection,
} from "@roost/shared/terminal-capture";
import type { TerminalIncidentBundleInput } from "../src/diag/terminal-capture-bundle-writer.ts";
import type { TerminalCaptureWorkerCommand } from "../src/diag/terminal-capture-write.ts";
import { SESSION_ID, STREAM_A } from "./terminal-stream-state-harness.ts";

export const FIXTURE_RECORDING_ID = "cccccccc-0000-4000-8000-00000000cccc";
export const FIXTURE_SESSION_ID = String(SESSION_ID);
export const FIXTURE_AT_MS = 1_700_000_000_000;

export const COMPLETE_COVERAGE: TerminalCaptureCoverageReport = {
	cell_replay: "complete",
	cell_replay_reasons: ["complete"],
	core_replay: "complete",
	core_replay_reasons: ["complete"],
	core_comparison: "complete",
	core_comparison_reasons: ["complete"],
};

export interface BrowserPaintedEvidence {
	readonly dom_history: readonly { readonly index: number }[];
	readonly gaps: readonly { readonly start: string; readonly end: string }[];
}

export function workerSectionFixture(
	overrides: Partial<TerminalWorkerSection> = {},
): TerminalWorkerSection {
	return {
		layer: "worker",
		captured_at_ms: FIXTURE_AT_MS,
		process: {
			layer: "worker",
			process_id: "dddddddd-0000-4000-8000-00000000dddd",
			git_sha: "test-sha",
			artifact_version: "test",
			wasm_identity: null,
			worker_fp: "00".repeat(32),
			viewer_id: null,
			user_agent: null,
		},
		stream: null,
		geometry: { cols: 80, rows: 24 },
		dropped: { records: 0, bytes: 0, rows: 0, raw_bytes: 0, samples: 0 },
		omissions: [],
		segments: [],
		emissions: [],
		core_samples: [],
		sampling: {
			sampled: 0,
			skipped_interval: 0,
			skipped_budget: 0,
			skipped_grid: 0,
			suppressed_until_ms: null,
			max_elapsed_us: 0,
		},
		resizes: [],
		raw: [],
		byte_capture: null,
		core_scrollback_tail: [],
		history_rows: [],
		history_ranges: [],
		scrollback_total: 0,
		scrollback_origin: "0",
		...overrides,
	};
}

export function incidentBundleInputFixture(
	captureId: string,
	worker: TerminalWorkerSection = workerSectionFixture(),
): TerminalIncidentBundleInput {
	const trigger: TerminalCaptureTrigger = {
		reason: "manual",
		origin: "worker",
		at_ms: FIXTURE_AT_MS,
		stream_id: null,
		grid_epoch: null,
		seq: null,
		detail: null,
		occurrence_count: 0,
	};
	return {
		capture_id: captureId,
		recording_id: FIXTURE_RECORDING_ID,
		session_id: FIXTURE_SESSION_ID,
		written_at_ms: FIXTURE_AT_MS,
		trigger,
		worker_trigger: trigger,
		coverage: COMPLETE_COVERAGE,
		worker,
		coordinator: null,
		browser: null,
	};
}

export function captureCommandFixture(
	overrides: Partial<TerminalCaptureWorkerCommand> = {},
): TerminalCaptureWorkerCommand {
	return {
		action: "capture",
		session_id: FIXTURE_SESSION_ID,
		recording_id: FIXTURE_RECORDING_ID,
		capture_id: "eeeeeeee-0000-4000-8000-00000000eeee",
		reason: "manual",
		browser_evidence_json: "",
		coordinator_evidence_json: "",
		...overrides,
	};
}

/** The real wire payload: the envelope carries capture identity and the
 *  trigger, and the SECTION is nested under a member named for its layer. */
export function browserPayload(
	captureId: string,
	painted: BrowserPaintedEvidence,
): TerminalCaptureBrowserPayload {
	return {
		schema: TERMINAL_INCIDENT_SCHEMA,
		layer: "browser",
		capture_id: captureId,
		recording_id: FIXTURE_RECORDING_ID,
		session_id: FIXTURE_SESSION_ID,
		trigger: {
			reason: "history_identity",
			origin: "browser",
			at_ms: FIXTURE_AT_MS,
			stream_id: STREAM_A,
			grid_epoch: "assembly:0",
			seq: "7",
			detail: "duplicate_history_index",
			occurrence_count: 3,
		},
		browser: {
			layer: "browser",
			captured_at_ms: FIXTURE_AT_MS,
			process: {
				layer: "browser",
				process_id: "ffffffff-0000-4000-8000-00000000ffff",
				git_sha: "test-sha",
				artifact_version: "test",
				wasm_identity: null,
				worker_fp: null,
				viewer_id: "viewer-1",
				user_agent: "test",
			},
			stream: null,
			geometry: null,
			dropped: { records: 0, bytes: 0, rows: 0, raw_bytes: 0, samples: 0 },
			omissions: [],
			events: [],
			replica: null,
			trigger_state: {
				at_ms: FIXTURE_AT_MS,
				phase: "pre_destructive",
				apply_mode: "full",
				canonical: null,
				committed: null,
				pending: null,
				painted_model_history: [],
				dom_history: painted.dom_history.map((row) => ({
					order: row.index,
					index: row.index,
					columns: 10,
					fingerprint: 0,
					text: "",
					span_count: 0,
				})),
				dom_viewport: [],
				gaps: painted.gaps.map((gap) => ({
					start: gap.start,
					end: gap.end,
					status: "unavailable" as const,
					rows: 0,
				})),
				cursor: null,
				scroll: null,
				reader: null,
				active: true,
				visible: true,
				omissions: [],
			},
			pre_repair_state: null,
			post_repair_state: null,
			current_state: null,
		},
	};
}

/** The coordinator's payload uses the identical nesting rule, under a member
 *  named for ITS layer. */
export function coordinatorPayload(captureId: string): TerminalCaptureCoordinatorPayload {
	return {
		schema: TERMINAL_INCIDENT_SCHEMA,
		layer: "coordinator",
		capture_id: captureId,
		recording_id: FIXTURE_RECORDING_ID,
		session_id: FIXTURE_SESSION_ID,
		coordinator: {
			layer: "coordinator",
			captured_at_ms: FIXTURE_AT_MS,
			process: {
				layer: "coordinator",
				process_id: "aaaaaaaa-0000-4000-8000-00000000ab01",
				git_sha: "test-sha",
				artifact_version: "test",
				wasm_identity: null,
				worker_fp: null,
				viewer_id: null,
				user_agent: null,
			},
			stream: null,
			geometry: null,
			dropped: { records: 0, bytes: 0, rows: 0, raw_bytes: 0, samples: 0 },
			omissions: [],
			records: [],
			snapshot: null,
			valid: true,
		},
	};
}

/** Correctly enveloped and correctly NESTED, but the section itself violates
 *  the bundle validator. The write gate must drop that layer and keep the
 *  worker's own evidence. */
export function malformedSectionPayload(captureId: string): Record<string, unknown> {
	const payload = browserPayload(captureId, { dom_history: [], gaps: [] });
	return { ...payload, browser: { ...payload.browser, events: "not-an-array" } };
}

/** Envelope- and nesting-valid with a SOUND section, but the peer-authored
 *  trigger it ships is structurally invalid. The envelope check proves only
 *  that a trigger is a plain object, so this reaches the write-side gate. */
export function invalidTriggerPayload(captureId: string): Record<string, unknown> {
	return { ...browserPayload(captureId, { dom_history: [], gaps: [] }), trigger: {} };
}

/** The exact production shape: the section's fields flattened onto the
 *  envelope, with no member named for the layer at all. */
export function flattenedBrowserPayload(captureId: string): Record<string, unknown> {
	const { browser, ...envelope } = browserPayload(captureId, { dom_history: [], gaps: [] });
	return { ...envelope, ...browser };
}
