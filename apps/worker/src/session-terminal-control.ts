// Entry points for coordinator terminal-control RPCs (stream open/close/
// resize): validates geometry, acquires the keeper admission lane so one
// control can never starve input, then delegates the core mutation to
// session-terminal-txn. This is where every downstream request's budget is
// enforced before any work is queued.
import { initCellEmitState } from "@roost/shared/cell";
import { TERMINAL_MAX_COLS, TERMINAL_MAX_ROWS } from "@roost/shared/viewport";
import { newTraceId } from "@roost/shared/trace";
import type { SessionManager } from "./session-manager.ts";
import type { TerminalRequestBudget } from "./transport/coord-link-types.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import { acquireKeeperAdmission, enqueueTerminalControl } from "./session-control-lanes.ts";
import { applyTerminalStreamNow } from "./session-terminal-txn.ts";
import { retireSnapshotCursor } from "./session-snapshot-cursor.ts";
import type {
	TerminalStreamState,
	WorkerTerminalStreamResult,
} from "./session-terminal-state.ts";

export type { WorkerTerminalStreamResult } from "./session-terminal-state.ts";

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

export type WorkerInputResult =
	| { status: "accepted"; writtenBytes: number }
	| { status: "rejected"; writtenBytes: 0; reason: string }
	| { status: "ambiguous"; writtenBytes: number; reason: string };

export interface WorkerTerminalStreamIntent {
	requestId: string;
	sessionId: string;
	streamId: string;
	enabled: boolean;
	cols: number;
	rows: number;
	budget?: TerminalRequestBudget;
}

export async function writeTerminalInput(
	this: SessionManager,
	sessionId: string,
	inputSeq: bigint,
	bytes: Uint8Array,
	budget?: TerminalRequestBudget,
): Promise<WorkerInputResult> {
	const rec = this.getBySessionId(sessionId);
	if (!rec) return { status: "rejected", writtenBytes: 0, reason: "session is not live" };
	if (inputSeq <= 0n) return { status: "rejected", writtenBytes: 0, reason: "input sequence must be positive" };
	if (bytes.byteLength === 0) return { status: "accepted", writtenBytes: 0 };
	const channelId = rec.channelId;
	const ticket = acquireKeeperAdmission(this, channelId, "terminal_input");
	const owned = bytes.slice();
	let command;
	try {
		await ticket.granted;
		if (!this.sessions.has(channelId)) {
			return { status: "rejected", writtenBytes: 0, reason: "session closed before the keeper write" };
		}
		if (budget && !budget.isCurrentConnection()) {
			return { status: "rejected", writtenBytes: 0, reason: "worker connection superseded before the keeper write" };
		}
		if (budget && budget.remainingMs() <= 0) {
			return { status: "rejected", writtenBytes: 0, reason: "input budget expired before the keeper write" };
		}
		this.markInputSensitive(channelId);
		command = getMultiplexedPool().beginInput(channelId, owned);
		if (!command.admission.written) {
			return { status: "rejected", writtenBytes: 0, reason: `keeper did not accept the input: ${command.admission.reason}` };
		}
	} catch (error) {
		return {
			status: "ambiguous",
			writtenBytes: 0,
			reason: error instanceof Error ? error.message : String(error),
		};
	} finally {
		ticket.release();
	}

	try {
		const result = await command.result;
		if (result.kind === "ack") {
			return result.writtenBytes === owned.byteLength
				? { status: "accepted", writtenBytes: result.writtenBytes }
				: { status: "ambiguous", writtenBytes: result.writtenBytes, reason: "keeper acknowledged an incomplete input batch" };
		}
		if (result.kind === "reject") {
			return { status: "rejected", writtenBytes: 0, reason: result.reason };
		}
		return { status: "ambiguous", writtenBytes: result.writtenBytes ?? 0, reason: result.reason };
	} catch (error) {
		return { status: "ambiguous", writtenBytes: 0, reason: error instanceof Error ? error.message : String(error) };
	}
}

function invalidResult(intent: WorkerTerminalStreamIntent, reason: string): WorkerTerminalStreamResult {
	return {
		status: "rejected",
		streamId: intent.streamId,
		enabled: intent.enabled,
		channelResizeSeq: 0,
		cols: intent.cols,
		rows: intent.rows,
		failure: "invalid_request",
		reason,
		phase: "pre_write",
	};
}

function statePayloadMatches(state: TerminalStreamState, intent: WorkerTerminalStreamIntent): boolean {
	return state.enabled === intent.enabled && state.cols === intent.cols && state.rows === intent.rows;
}

function settledStateResult(
	state: TerminalStreamState,
	channelResizeSeq: number,
): WorkerTerminalStreamResult {
	if (!state.coreValid) {
		return {
			status: "ambiguous",
			streamId: state.streamId,
			enabled: state.enabled,
			channelResizeSeq,
			cols: state.cols,
			rows: state.rows,
			failure: "core_failed",
			reason: "terminal core is invalid and requires adoption",
			phase: "pre_write",
		};
	}
	return {
		status: "committed",
		streamId: state.streamId,
		enabled: state.enabled,
		channelResizeSeq,
		cols: state.enabled ? state.cols : 0,
		rows: state.enabled ? state.rows : 0,
		resized: false,
		phase: "written",
	};
}

export function applyTerminalStreamState(
	this: SessionManager,
	intent: WorkerTerminalStreamIntent,
): Promise<WorkerTerminalStreamResult> {
	if (!UUID_RE.test(intent.sessionId)) return Promise.resolve(invalidResult(intent, "session_id must be a UUID"));
	if (!UUID_RE.test(intent.streamId)) return Promise.resolve(invalidResult(intent, "stream_id must be a UUID"));
	if (intent.requestId.length === 0 || intent.requestId.length > 128) {
		return Promise.resolve(invalidResult(intent, "request_id is invalid"));
	}
	if (intent.enabled) {
		if (!Number.isInteger(intent.cols) || intent.cols < 1 || intent.cols > TERMINAL_MAX_COLS
			|| !Number.isInteger(intent.rows) || intent.rows < 1 || intent.rows > TERMINAL_MAX_ROWS) {
			return Promise.resolve(invalidResult(
				intent,
				`enabled geometry must be within 1..${TERMINAL_MAX_COLS} cols and 1..${TERMINAL_MAX_ROWS} rows`,
			));
		}
	} else if (intent.cols !== 0 || intent.rows !== 0) {
		return Promise.resolve(invalidResult(intent, "disabled terminal stream must have zero geometry"));
	}
	const rec = this.getBySessionId(intent.sessionId);
	if (!rec) {
		return Promise.resolve({
			...invalidResult(intent, "session is not live"),
			failure: "session_not_live",
		});
	}
	const channelId = rec.channelId;
	const current = this.terminalStreams.get(channelId);
	if (current?.streamId === intent.streamId) {
		if (!statePayloadMatches(current, intent)) {
			return Promise.resolve(invalidResult(intent, "stream_id was reused with a conflicting payload"));
		}
		return current.operation
			?? Promise.resolve(settledStateResult(current, this.channelResizeSeq.get(channelId) ?? 0));
	}
	if (current) retireSnapshotCursor(this, channelId, current);

	const baseline = Promise.withResolvers<boolean>();
	const next: TerminalStreamState = {
		streamId: intent.streamId,
		enabled: intent.enabled,
		cols: intent.cols,
		rows: intent.rows,
		version: this.nextTerminalStreamVersion(),
		baselineReady: !intent.enabled,
		coreValid: current?.coreValid ?? true,
		baselineDirty: false,
		snapshotCursor: null,
		resizeCapture: current?.resizeCapture ?? null,
		baselineInstalled: baseline.promise,
		baselinePromisePending: intent.enabled,
		resolveBaselineInstalled: baseline.resolve,
	};
	if (!intent.enabled) baseline.resolve(true);
	this.terminalStreams.set(channelId, next);
	// Stream generations own sequence space, not grid identity. A reconnect or
	// renewed viewer membership over the same core/geometry must keep the epoch
	// so a warm renderer can merge the new viewport baseline into its retained
	// history. Only a real geometry change invalidates row identity.
	const previousEmit = rec.cell_emit;
	const appliedSize = this.lastAppliedSize.get(channelId);
	const geometryChanges = intent.enabled && appliedSize !== undefined
		&& (appliedSize.cols !== intent.cols || appliedSize.rows !== intent.rows);
	rec.cell_emit = {
		...initCellEmitState(previousEmit.gridEpochBase, intent.streamId),
		gridEpochRevision: previousEmit.gridEpochRevision + (geometryChanges ? 1 : 0),
		lastSbTotal: previousEmit.lastSbTotal,
		sentFull: previousEmit.sentFull,
		cols: previousEmit.cols,
		rows: previousEmit.rows,
		alt: previousEmit.alt,
		sbOrigin: previousEmit.sbOrigin,
		sbDropped: previousEmit.sbDropped,
	};
	const ticket = acquireKeeperAdmission(this, channelId, "terminal_resize");
	const operation = enqueueTerminalControl(
		this,
		channelId,
		"terminal_stream",
		() => applyTerminalStreamNow(this, channelId, next, intent.budget, ticket),
	).finally(() => ticket.release());
	next.operation = operation;
	return operation;
}

export function requestTerminalSnapshot(
	this: SessionManager,
	sessionId: string,
	streamId: string,
): void {
	const rec = this.getBySessionId(sessionId);
	if (!rec) return;
	const state = this.terminalStreams.get(rec.channelId);
	if (!state || !state.enabled || !state.coreValid || state.streamId !== streamId) return;
	retireSnapshotCursor(this, rec.channelId, state);
	const baseline = Promise.withResolvers<boolean>();
	state.baselineReady = false;
	state.baselineDirty = false;
	state.baselineInstalled = baseline.promise;
	state.resolveBaselineInstalled = baseline.resolve;
	state.baselinePromisePending = true;
	this.installTerminalBaseline(rec.channelId);
}
