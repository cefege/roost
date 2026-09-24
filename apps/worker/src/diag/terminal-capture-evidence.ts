// Remote evidence handling for one CAPTURE: envelope-check the already
// size-bounded browser and coordinator JSON, and derive from it the exact
// absolute history rows the worker should read back. Called by
// diag/terminal-capture.ts before it freezes worker evidence.
// Nothing here trusts a remote field for anything but a bounded row range —
// the destination path, the session and the recording are all worker- or
// coordinator-proved before this runs.

import {
	checkTerminalCaptureEnvelope,
	TERMINAL_CAPTURE_LIMITS,
	type TerminalCaptureCommand,
	type TerminalCaptureErrorCode,
	type TerminalCaptureLayer,
	type TerminalCaptureTrigger,
} from "@roost/protocol/terminal-capture";
import type { WorkerHistoryRequest } from "./terminal-capture-worker-section.ts";

/** Painted states a browser section carries; each names DOM history rows and
 *  the gaps around them, which is what the worker reads back. */
const BROWSER_STATE_FIELDS = [
	"trigger_state",
	"pre_repair_state",
	"post_repair_state",
	"current_state",
] as const;

/** Disjoint ranges one browser may name. Each requested range can be reported
 *  as up to THREE entries (its evicted prefix, its present body, its
 *  unavailable suffix), and `worker.history_ranges` is validated against
 *  `layerEntries` — so a looser cap here would make the bundle unwritable. */
const MAX_EVIDENCE_RANGES = Math.floor(TERMINAL_CAPTURE_LIMITS.layerEntries / 3);

export type RemoteEvidence =
	| {
			readonly ok: true;
			/** The layer's own section, unwrapped from its envelope. */
			readonly section: Record<string, unknown> | null;
			readonly trigger: TerminalCaptureTrigger | null;
		}
	| { readonly ok: false; readonly code: TerminalCaptureErrorCode };

/** Envelope-check one layer's payload and return the NESTED section, never the
 *  envelope: the envelope carries the capture identity that the bundle already
 *  holds at top level, and placing it where a section belongs validates as an
 *  envelope and then fails as a section — dropping that layer's whole evidence
 *  with nothing but an omission to show for it.
 *  An empty payload is a legitimate absence (a worker-local trigger has no
 *  remote evidence at all), not an error. */
export function parseRemoteEvidence(
	json: string,
	layer: TerminalCaptureLayer,
	command: TerminalCaptureCommand,
): RemoteEvidence {
	if (json.length === 0) return { ok: true, section: null, trigger: null };
	const checked = checkTerminalCaptureEnvelope(json, { layer, command });
	if (!checked.ok) return { ok: false, code: checked.code };
	return { ok: true, section: checked.section, trigger: checked.trigger };
}

/** The absolute history rows the browser evidence named, as coalesced ranges.
 *  Empty when there is no browser evidence, in which case the worker reads its
 *  own newest tail instead. */
export function historyRangesFromBrowserEvidence(
	browser: Record<string, unknown> | null,
): WorkerHistoryRequest[] {
	if (!browser) return [];
	const indices: number[] = [];
	const ranges: WorkerHistoryRequest[] = [];
	for (const field of BROWSER_STATE_FIELDS) {
		const state = browser[field];
		if (state === null || typeof state !== "object" || Array.isArray(state)) continue;
		collectDomHistoryIndices(state as Record<string, unknown>, indices);
		collectGapRanges(state as Record<string, unknown>, ranges);
	}
	for (const range of coalesceIndices(indices)) {
		if (ranges.length >= MAX_EVIDENCE_RANGES) break;
		ranges.push(range);
	}
	ranges.sort((left, right) => left.start - right.start);
	return ranges.slice(0, MAX_EVIDENCE_RANGES);
}

function collectDomHistoryIndices(
	state: Record<string, unknown>,
	indices: number[],
): void {
	const rows = state.dom_history;
	if (!Array.isArray(rows)) return;
	for (const raw of rows) {
		if (indices.length >= TERMINAL_CAPTURE_LIMITS.browserRowsMax) return;
		if (raw === null || typeof raw !== "object") continue;
		const index = (raw as Record<string, unknown>).index;
		if (typeof index === "number" && Number.isSafeInteger(index) && index >= 0) {
			indices.push(index);
		}
	}
}

function collectGapRanges(
	state: Record<string, unknown>,
	ranges: WorkerHistoryRequest[],
): void {
	const gaps = state.gaps;
	if (!Array.isArray(gaps)) return;
	for (const raw of gaps) {
		if (ranges.length >= MAX_EVIDENCE_RANGES) return;
		if (raw === null || typeof raw !== "object") continue;
		const gap = raw as Record<string, unknown>;
		const start = safeRowIndex(gap.start);
		const end = safeRowIndex(gap.end);
		if (start === null || end === null || end <= start) continue;
		ranges.push({ start, end: Math.min(end, start + TERMINAL_CAPTURE_LIMITS.captureHistoryRows) });
	}
}

/** Absolute row indices are decimal STRINGS on the wire (a JSON number loses
 *  exactness past 2^53) but a row index the worker can actually read is always
 *  inside the safe-integer range, so anything beyond it is not a row request. */
function safeRowIndex(value: unknown): number | null {
	if (typeof value === "number") {
		return Number.isSafeInteger(value) && value >= 0 ? value : null;
	}
	if (typeof value !== "string" || !/^(0|[1-9][0-9]{0,15})$/.test(value)) return null;
	const parsed = Number(value);
	return Number.isSafeInteger(parsed) ? parsed : null;
}

function coalesceIndices(indices: number[]): WorkerHistoryRequest[] {
	if (indices.length === 0) return [];
	indices.sort((left, right) => left - right);
	const ranges: WorkerHistoryRequest[] = [];
	let start = indices[0]!;
	let end = start + 1;
	for (const index of indices) {
		if (index <= end) {
			end = Math.max(end, index + 1);
			continue;
		}
		ranges.push({ start, end });
		if (ranges.length >= MAX_EVIDENCE_RANGES) return ranges;
		start = index;
		end = index + 1;
	}
	ranges.push({ start, end });
	return ranges;
}
