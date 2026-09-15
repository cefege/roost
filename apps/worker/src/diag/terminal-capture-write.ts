// Assembles and writes ONE incident bundle, for both a coordinator-driven
// CAPTURE and a worker-local emission conflict. Called by
// diag/terminal-capture.ts; the freeze it performs is synchronous and the
// compression/write it awaits is not, so the live PTY never waits for disk.
// Remote sections are used exactly as the peers froze them, or reported
// explicitly absent — this module never fabricates a layer it did not receive.

import { randomUUID } from "node:crypto";
import { existsSync } from "node:fs";
import { signal } from "@roost/shared/diag";
import {
	TERMINAL_CAPTURE_LIMITS,
	type TerminalCaptureCommand,
	type TerminalCaptureCoverageReport,
	type TerminalCaptureFileRef,
	type TerminalCaptureStatus,
	type TerminalCaptureTrigger,
	type TerminalCoordinatorSection,
} from "@roost/shared/terminal-capture";
import type { SessionManager } from "../session-manager.ts";
import type { SessionRecord } from "../session-record.ts";
import type { TerminalStreamState } from "../session-terminal-state.ts";
import {
	recentWorkerCaptureFor,
	terminalCaptureFailureAck,
	type TerminalCaptureWorkerAck,
} from "./terminal-capture-ack.ts";
import { writeTerminalIncidentBundle } from "./terminal-capture-bundle-writer.ts";
import {
	historyRangesFromBrowserEvidence,
	parseRemoteEvidence,
} from "./terminal-capture-evidence.ts";
import {
	activeRecorder,
	oneShotLedger,
} from "./terminal-capture-registry.ts";
import type {
	CaptureLedger,
	WorkerCaptureRecorder,
} from "./terminal-capture-recorder.ts";
import {
	freezeWorkerSection,
	type FrozenWorkerSection,
} from "./terminal-capture-worker-section.ts";

/** One worker wire command. `coordinator_evidence_json` is worker-side only:
 *  the coordinator supplies its own evidence, never the browser. */
export interface TerminalCaptureWorkerCommand extends TerminalCaptureCommand {
	readonly coordinator_evidence_json: string;
}

export interface TerminalCaptureWorkerDeps {
	readonly sessionMgr: SessionManager;
}

/** Worker-local captures whose write has not settled. Bounded by the
 *  automatic latch and the session-wide floor, and drained by the diagnostic
 *  settle seam below. */
const _scheduledCaptures = new Set<Promise<void>>();

/** Freeze this session's worker evidence, merge the already-frozen remote
 *  evidence, and write ONE bundle. Everything up to the freeze runs in the
 *  caller's synchronous turn. */
export async function captureTerminalIncident(
	command: TerminalCaptureWorkerCommand,
	deps: TerminalCaptureWorkerDeps,
): Promise<TerminalCaptureWorkerAck> {
	const nowMs = Date.now();
	const recorder = activeRecorder(command.session_id);
	const ledger = recorder?.ledger ?? oneShotLedger(command.session_id);
	const prior = ledger.completed.get(command.capture_id);
	if (prior) return replayCompletedCapture(prior, command.capture_id, ledger, recorder);
	const recent = recentWorkerCaptureFor(ledger.recent_worker_local, command.capture_id);
	if (recorder?.capture_in_flight) {
		return terminalCaptureFailureAck("capture_in_flight", recent, recorder.expires_at_ms);
	}
	if (ledger.completed.size >= TERMINAL_CAPTURE_LIMITS.completedCaptureIds) {
		return terminalCaptureFailureAck("resource_exhausted", recent, null);
	}
	if (
		command.reason === "manual"
		&& nowMs - ledger.last_manual_ms < TERMINAL_CAPTURE_LIMITS.manualCooldownMs
	) {
		return terminalCaptureFailureAck("rate_limited", recent, null);
	}
	const record = deps.sessionMgr.getBySessionId(command.session_id);
	if (!record) return terminalCaptureFailureAck("session_unknown", recent, null);

	const browser = parseRemoteEvidence(command.browser_evidence_json, "browser", command);
	if (!browser.ok) return terminalCaptureFailureAck(browser.code, recent, null);
	const coordinator = parseRemoteEvidence(
		command.coordinator_evidence_json,
		"coordinator",
		command,
	);
	if (!coordinator.ok) {
		return terminalCaptureFailureAck(coordinator.code, recent, null);
	}

	const frozen = freezeWorkerSection({
		session_id: command.session_id,
		worker_fp: recorder?.worker_fp ?? String(deps.sessionMgr.workerFp),
		recorder,
		record,
		stream: deps.sessionMgr.terminalStreams.get(record.channelId),
		history_ranges: historyRangesFromBrowserEvidence(browser.section),
		captured_at_ms: nowMs,
	});
	if (recorder) recorder.capture_in_flight = true;
	if (command.reason === "manual") ledger.last_manual_ms = nowMs;
	const workerTrigger: TerminalCaptureTrigger = {
		reason: command.reason,
		// `origin: "worker"` means the worker detected this incident itself, so
		// a requested capture never carries it: the request came down the
		// coordinator's wire, whether or not the browser reached it in time.
		origin: browser.section === null ? "coordinator" : "browser",
		at_ms: nowMs,
		stream_id: frozen.section.stream?.stream_id ?? null,
		grid_epoch: frozen.section.stream?.grid_epoch ?? null,
		seq: frozen.section.stream?.seq ?? null,
		detail: null,
		occurrence_count: 0,
	};
	return finishCapture({
		capture_id: command.capture_id,
		recording_id: command.recording_id,
		session_id: command.session_id,
		ledger,
		recorder,
		frozen,
		// A coordinator-driven CAPTURE is requested evidence, never an incident
		// the worker detected on its own, whatever the peer's trigger claims.
		worker_local: false,
		// The browser authors the trigger when it is the origin: only it knows
		// WHICH invariant fired and how many same-identity occurrences the latch
		// collapsed. Synthesizing one here would erase `detail` and report every
		// automatic capture as occurrence 0. It is untrusted, so the write gate
		// keeps `workerTrigger` to fall back on — and `origin` stays worker-owned,
		// because the authoring layer is a structural fact, not a peer's claim.
		trigger: browser.trigger === null
			? workerTrigger
			: { ...browser.trigger, origin: "browser" },
		worker_trigger: workerTrigger,
		coordinator: coordinator.section as TerminalCoordinatorSection | null,
		browser: browser.section,
	});
}

/** Freeze inside the emission turn and let only the compression and the write
 *  run later. The remote layers are explicitly absent: a worker-local trigger
 *  has no browser or coordinator snapshot. */
export function scheduleWorkerLocalCapture(
	recorder: WorkerCaptureRecorder,
	record: SessionRecord,
	stream: TerminalStreamState,
	conflict: { readonly seq: string; readonly latch_key: string },
	nowMs: number,
): void {
	const captureId = randomUUID();
	const frozen = freezeWorkerSection({
		session_id: recorder.session_id,
		worker_fp: recorder.worker_fp,
		recorder,
		record,
		stream,
		history_ranges: [],
		captured_at_ms: nowMs,
	});
	recorder.capture_in_flight = true;
	const trigger: TerminalCaptureTrigger = {
		reason: "worker_emission",
		origin: "worker",
		at_ms: nowMs,
		stream_id: stream.streamId,
		grid_epoch: frozen.section.stream?.grid_epoch ?? null,
		seq: conflict.seq,
		detail: "core_fold_disagreement",
		occurrence_count: recorder.occurrences.get(conflict.latch_key) ?? 1,
	};
	const entry = new Promise<void>((resolve) => {
		setImmediate(() => {
			void finishCapture({
				capture_id: captureId,
				recording_id: recorder.recording_id,
				session_id: recorder.session_id,
				ledger: recorder.ledger,
				recorder,
				frozen,
				worker_local: true,
				trigger,
				worker_trigger: trigger,
				coordinator: null,
				browser: null,
			}).catch(() => {
				recorder.capture_in_flight = false;
				signal("terminal.capture_failed", {
					sid: recorder.session_id,
					capture_id: captureId,
					error: "internal",
					cooldownKey: captureId,
				});
			}).finally(() => {
				// Self-removal is what keeps this set bounded: production never
				// calls the settle seam below.
				_scheduledCaptures.delete(entry);
				resolve();
			});
		});
	});
	_scheduledCaptures.add(entry);
}

/** Diagnostic seam: settle every worker-local capture currently in flight.
 *  Tests await the actual write instead of a wall-clock guess. */
export async function _settleScheduledCaptures(): Promise<void> {
	while (_scheduledCaptures.size > 0) await Promise.all([..._scheduledCaptures]);
}

interface FinishCaptureRequest {
	readonly capture_id: string;
	readonly recording_id: string;
	readonly session_id: string;
	readonly ledger: CaptureLedger;
	readonly recorder: WorkerCaptureRecorder | null;
	readonly frozen: FrozenWorkerSection;
	/** TRUE only for the emission-conflict path the worker itself triggered.
	 *  It alone may advance `recent_worker_local`, so a requested capture can
	 *  never be reported back as a worker-detected incident. */
	readonly worker_local: boolean;
	readonly trigger: TerminalCaptureTrigger;
	/** Worker-owned fallback when `trigger` came from a peer. */
	readonly worker_trigger: TerminalCaptureTrigger;
	readonly coordinator: TerminalCoordinatorSection | null;
	readonly browser: Record<string, unknown> | null;
}

async function finishCapture(
	request: FinishCaptureRequest,
): Promise<TerminalCaptureWorkerAck> {
	const written = await writeTerminalIncidentBundle({
		capture_id: request.capture_id,
		recording_id: request.recording_id,
		session_id: request.session_id,
		written_at_ms: Date.now(),
		trigger: request.trigger,
		worker_trigger: request.worker_trigger,
		coverage: request.frozen.coverage,
		worker: request.frozen.section,
		coordinator: request.coordinator,
		browser: request.browser,
	});
	if (request.recorder) request.recorder.capture_in_flight = false;
	if (!written.ok) {
		signal("terminal.capture_failed", {
			sid: request.session_id,
			capture_id: request.capture_id,
			error: written.code,
			cooldownKey: request.capture_id,
		});
		return terminalCaptureFailureAck(
			written.code,
			recentWorkerCaptureFor(request.ledger.recent_worker_local, request.capture_id),
			request.recorder?.expires_at_ms ?? null,
		);
	}
	const status = captureStatusOf(request, written.trimmed);
	const ref: TerminalCaptureFileRef = {
		capture_id: request.capture_id,
		path: written.path,
		byte_length: written.byte_length,
		status,
	};
	if (request.worker_local) request.ledger.recent_worker_local = ref;
	const ack: TerminalCaptureWorkerAck = {
		status,
		path: written.path,
		byte_length: written.byte_length,
		error: null,
		expires_at_ms: request.recorder?.expires_at_ms ?? null,
		recent_worker_capture: recentWorkerCaptureFor(
			request.ledger.recent_worker_local,
			request.capture_id,
		),
	};
	request.ledger.completed.set(request.capture_id, ack);
	signal("terminal.capture_saved", {
		sid: request.session_id,
		capture_id: request.capture_id,
		byte_len: written.byte_length,
		reason: request.trigger.reason,
		status,
		cell_replay: request.frozen.coverage.cell_replay,
		core_replay: request.frozen.coverage.core_replay,
		core_comparison: request.frozen.coverage.core_comparison,
		cooldownKey: request.capture_id,
	});
	return ack;
}

/** `captured` claims every layer present and every replay complete. Anything
 *  else — a trimmed export, a missing remote section, partial coverage — is
 *  reported as `partial` so a reader never treats it as a whole picture. */
function captureStatusOf(
	request: FinishCaptureRequest,
	trimmed: boolean,
): TerminalCaptureStatus {
	if (trimmed || request.coordinator === null || request.browser === null) return "partial";
	return isCompleteCoverage(request.frozen.coverage) ? "captured" : "partial";
}

function isCompleteCoverage(coverage: TerminalCaptureCoverageReport): boolean {
	return coverage.cell_replay === "complete"
		&& coverage.core_replay === "complete"
		&& coverage.core_comparison === "complete";
}

/** An RPC retry gets the ORIGINAL result. When retention already removed that
 *  file the answer is `capture_expired`: recreating it from later terminal
 *  state would silently substitute a different incident. */
function replayCompletedCapture(
	prior: TerminalCaptureWorkerAck,
	captureId: string,
	ledger: CaptureLedger,
	recorder: WorkerCaptureRecorder | null,
): TerminalCaptureWorkerAck {
	if (prior.path === null || existsSync(prior.path)) return prior;
	return terminalCaptureFailureAck(
		"capture_expired",
		recentWorkerCaptureFor(ledger.recent_worker_local, captureId),
		recorder?.expires_at_ms ?? null,
	);
}
