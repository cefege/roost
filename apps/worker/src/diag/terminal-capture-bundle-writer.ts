// Turns the three frozen layer sections into ONE owner-only
// terminal-incident-<capture-id>.json.gz: fit the uncompressed budget by
// dropping whole sections and rows (never by truncating a string), run the
// shared bundle validator as a write-side gate, gzip, and hand the bytes to
// diag/capture-storage.ts. Called by diag/terminal-capture.ts, off the PTY
// path; the WASM identity stamped here is a boot constant, not evidence.

import { gzipSync } from "node:zlib";
import { diag } from "@roost/observability/diag";
import { expectedRoostWasmSha256 } from "@roost/wterm/wterm-wasm";
import {
	TERMINAL_CAPTURE_LIMITS,
	TERMINAL_INCIDENT_SCHEMA,
	utf8ByteLength,
	type TerminalCaptureCoverageReport,
	type TerminalCaptureErrorCode,
	type TerminalCaptureOmission,
	type TerminalCaptureTrigger,
	type TerminalCoordinatorSection,
	type TerminalCoverageReason,
	type TerminalIncidentBundle,
	type TerminalWorkerSection,
} from "@roost/protocol/terminal-capture";
import { validateTerminalIncidentBundle } from "@roost/protocol/terminal-capture-validate";
import { writeTerminalIncidentFile } from "./capture-storage.ts";

export interface TerminalIncidentBundleInput {
	readonly capture_id: string;
	readonly recording_id: string;
	readonly session_id: string;
	readonly written_at_ms: number;
	/** Preferred trigger. May be PEER-authored: the envelope check proves only
	 *  that it is a plain object, so it can still fail the write-side gate. */
	readonly trigger: TerminalCaptureTrigger;
	/** Worker-owned trigger, always structurally valid. The remote-drop retry
	 *  falls back to it, because a peer-authored trigger belongs to the same
	 *  payload that just failed validation. */
	readonly worker_trigger: TerminalCaptureTrigger;
	readonly coverage: TerminalCaptureCoverageReport;
	readonly worker: TerminalWorkerSection;
	readonly coordinator: TerminalCoordinatorSection | null;
	/** Already envelope-checked browser evidence, parsed from its JSON. */
	readonly browser: Record<string, unknown> | null;
}

export type TerminalIncidentWriteResult =
	| {
		readonly ok: true;
		readonly path: string;
		readonly byte_length: number;
		readonly trimmed: boolean;
	}
	| { readonly ok: false; readonly code: TerminalCaptureErrorCode };

/** The worker evidence arrays a trim drops, largest and least attributive
 *  first: raw bytes replay nothing without a complete prefix, while the
 *  emissions and the trigger's own samples are the attribution itself. */
const TRIM_ORDER = [
	"raw",
	"core_scrollback_tail",
	"history_rows",
	"core_samples",
	"emissions",
] as const;

let _wasmIdentity: Promise<string | null> | null = null;

/** The pinned core's committed digest. Cached per process; a bundle whose
 *  worker cannot name its own WASM cannot be compared against another host's. */
export function workerWasmIdentity(): Promise<string | null> {
	_wasmIdentity ??= expectedRoostWasmSha256().catch(() => null);
	return _wasmIdentity;
}

export async function writeTerminalIncidentBundle(
	input: TerminalIncidentBundleInput,
): Promise<TerminalIncidentWriteResult> {
	// Yield once so compression and the disk write never share a turn with the
	// PTY read that produced the evidence.
	await new Promise<void>((resolve) => { setImmediate(resolve); });
	const wasmIdentity = await workerWasmIdentity();

	const worker: TerminalWorkerSection = {
		...input.worker,
		process: { ...input.worker.process, wasm_identity: wasmIdentity },
	};
	let fitted = fitBundleBudget({ ...input, worker });
	let validation = validateTerminalIncidentBundle(fitted.bundle);
	if (!validation.ok && (input.coordinator !== null || input.browser !== null)) {
		// Everything the peer authored — both sections AND the trigger it shipped
		// with them — is dropped together. Keeping the peer trigger here would
		// fail the retry identically and cost the worker its whole section.
		diag("diag.terminal_capture_remote_section_dropped", {
			capture_id: input.capture_id,
			code: validation.code,
			field: validation.field,
		});
		fitted = fitBundleBudget({
			...input,
			trigger: input.worker_trigger,
			worker: {
				...worker,
				omissions: [...worker.omissions, remoteSectionOmission(validation.field)],
			},
			coordinator: null,
			browser: null,
		});
		validation = validateTerminalIncidentBundle(fitted.bundle);
	}
	if (!validation.ok) {
		diag("diag.terminal_capture_invalid_bundle", {
			capture_id: input.capture_id,
			code: validation.code,
			field: validation.field,
		});
		return { ok: false, code: "internal" };
	}
	let payload: Uint8Array;
	try {
		payload = gzipSync(Buffer.from(fitted.json, "utf8"));
	} catch {
		return { ok: false, code: "storage_failed" };
	}
	const written = writeTerminalIncidentFile(input.capture_id, payload);
	if (!written.ok) return written;
	return {
		ok: true,
		path: written.path,
		byte_length: written.byte_length,
		trimmed: fitted.trimmed,
	};
}

interface FittedBundle {
	readonly bundle: TerminalIncidentBundle;
	readonly json: string;
	readonly trimmed: boolean;
}

/** Drop whole sections and whole row/record arrays until the UNCOMPRESSED JSON
 *  fits. Each drop is named in the layer's omissions and downgrades coverage:
 *  a trimmed export is partial, never "complete". */
function fitBundleBudget(input: TerminalIncidentBundleInput): FittedBundle {
	let bundle = assembleBundle(input, input.worker, input.coordinator, input.browser, []);
	let json = JSON.stringify(bundle);
	if (utf8ByteLength(json) <= TERMINAL_CAPTURE_LIMITS.bundleBytes) {
		return { bundle, json, trimmed: false };
	}

	const omissions: TerminalCaptureOmission[] = [];
	let worker = input.worker;
	let coordinator = input.coordinator;
	let browser = input.browser;
	for (const field of TRIM_ORDER) {
		const dropped = worker[field].length;
		if (dropped === 0) continue;
		worker = { ...worker, [field]: [] };
		omissions.push(trimOmission(`worker.${field}`, dropped));
		bundle = assembleBundle(input, worker, coordinator, browser, omissions);
		json = JSON.stringify(bundle);
		if (utf8ByteLength(json) <= TERMINAL_CAPTURE_LIMITS.bundleBytes) {
			return { bundle, json, trimmed: true };
		}
	}
	if (worker.byte_capture !== null) {
		worker = { ...worker, byte_capture: null };
		omissions.push(trimOmission("worker.byte_capture", 1));
		bundle = assembleBundle(input, worker, coordinator, browser, omissions);
		json = JSON.stringify(bundle);
		if (utf8ByteLength(json) <= TERMINAL_CAPTURE_LIMITS.bundleBytes) {
			return { bundle, json, trimmed: true };
		}
	}
	// The remote sections are the last to go: without them the bundle still
	// attributes the worker's own layer, which is what this file is for.
	for (const drop of ["coordinator", "browser"] as const) {
		if (drop === "coordinator") {
			if (coordinator === null) continue;
			coordinator = null;
		} else {
			if (browser === null) continue;
			browser = null;
		}
		omissions.push(trimOmission(drop, 1));
		bundle = assembleBundle(input, worker, coordinator, browser, omissions);
		json = JSON.stringify(bundle);
		if (utf8ByteLength(json) <= TERMINAL_CAPTURE_LIMITS.bundleBytes) {
			return { bundle, json, trimmed: true };
		}
	}
	return { bundle, json, trimmed: true };
}

function assembleBundle(
	input: TerminalIncidentBundleInput,
	worker: TerminalWorkerSection,
	coordinator: TerminalCoordinatorSection | null,
	browser: Record<string, unknown> | null,
	trimOmissions: readonly TerminalCaptureOmission[],
): TerminalIncidentBundle {
	const workerSection: TerminalWorkerSection = trimOmissions.length === 0
		? worker
		: { ...worker, omissions: [...worker.omissions, ...trimOmissions] };
	return {
		schema: TERMINAL_INCIDENT_SCHEMA,
		capture_id: input.capture_id,
		recording_id: input.recording_id,
		session_id: input.session_id,
		written_at_ms: input.written_at_ms,
		trigger: input.trigger,
		coverage: trimOmissions.length === 0
			? input.coverage
			: downgradeCoverage(input.coverage),
		browser: browser as TerminalIncidentBundle["browser"],
		coordinator,
		worker: workerSection,
	};
}

function downgradeCoverage(
	coverage: TerminalCaptureCoverageReport,
): TerminalCaptureCoverageReport {
	return {
		cell_replay: coverage.cell_replay === "unavailable" ? "unavailable" : "partial",
		cell_replay_reasons: withTrimReason(coverage.cell_replay_reasons),
		core_replay: coverage.core_replay === "unavailable" ? "unavailable" : "partial",
		core_replay_reasons: withTrimReason(coverage.core_replay_reasons),
		core_comparison: coverage.core_comparison === "unavailable" ? "unavailable" : "partial",
		core_comparison_reasons: withTrimReason(coverage.core_comparison_reasons),
	};
}

function withTrimReason(
	reasons: readonly TerminalCoverageReason[],
): readonly TerminalCoverageReason[] {
	const kept = reasons.filter((reason) => reason !== "complete");
	return [...kept, "evidence_trimmed"];
}

function trimOmission(name: string, droppedCount: number): TerminalCaptureOmission {
	return {
		kind: name === "coordinator" || name === "browser" ? "section" : "records",
		name,
		reason: "evidence_trimmed",
		dropped_count: droppedCount,
		dropped_bytes: 0,
		range: null,
	};
}

/** The remote layers were received but did not survive validation. `field` is a
 *  validator field PATH, never a value, so it carries no terminal content. */
function remoteSectionOmission(field: string): TerminalCaptureOmission {
	return {
		kind: "section",
		name: `remote:${field}`,
		reason: "layer_unavailable",
		dropped_count: 1,
		dropped_bytes: 0,
		range: null,
	};
}
