// Terminal control transactions: applies a coordinator stream request to a
// live core exactly once, under an admission ticket, with an ambiguous-
// boundary protocol — when the core's validity changes mid-transaction the
// outcome is reported as ambiguous rather than guessed. Called from
// session-terminal-control.ts.
import type { SessionManager } from "./session-manager.ts";
import type { TerminalRequestBudget } from "./transport/coord-link-types.ts";
import type { KeeperAdmissionTicket } from "./session-control-lanes.ts";
import type {
	LiveResizeCapture,
	TerminalStreamFailure,
	TerminalStreamState,
	WorkerTerminalStreamResult,
} from "./session-terminal-state.ts";
import {
	getMultiplexedPool,
	type KeeperCommand,
	type KeeperResizeResult,
} from "./keeper/multiplexed-client.ts";
import {
	applyResizeResultAtBoundary,
	installLiveResizeCapture,
	recoverAmbiguousResize,
} from "./session-resize-capture.ts";

const STREAM_APPLICATION_BUDGET_MS = 7_500;

function failed(
	state: TerminalStreamState,
	channelResizeSeq: number,
	failure: TerminalStreamFailure,
	reason: string,
	status: "rejected" | "ambiguous" = "rejected",
	phase: "pre_write" | "written" | "unknown" = status === "rejected" ? "pre_write" : "unknown",
): WorkerTerminalStreamResult {
	return {
		status,
		streamId: state.streamId,
		enabled: state.enabled,
		channelResizeSeq,
		cols: state.cols,
		rows: state.rows,
		failure,
		reason,
		phase,
	};
}

function committed(
	state: TerminalStreamState,
	channelResizeSeq: number,
	resized: boolean,
): WorkerTerminalStreamResult {
	return {
		status: "committed",
		streamId: state.streamId,
		enabled: state.enabled,
		channelResizeSeq,
		cols: state.enabled ? state.cols : 0,
		rows: state.enabled ? state.rows : 0,
		resized,
		phase: "written",
	};
}

async function awaitBaselineInstallation(state: TerminalStreamState): Promise<boolean> {
	if (state.snapshotCursor || state.baselineReady) return true;
	const timeout = Promise.withResolvers<boolean>();
	const timer = setTimeout(() => timeout.resolve(false), STREAM_APPLICATION_BUDGET_MS);
	const installed = await Promise.race([
		state.baselineInstalled.then(() => true),
		timeout.promise,
	]);
	clearTimeout(timer);
	return installed;
}

/** Apply one already-aggregated coordinator stream state. The terminal control
 * lane serializes calls; a newer state object supersedes this one's cell work
 * immediately, while any keeper mutation already written is still reconciled. */
export async function applyTerminalStreamNow(
	mgr: SessionManager,
	channelId: number,
	state: TerminalStreamState,
	budget: TerminalRequestBudget | undefined,
	ticket: KeeperAdmissionTicket,
): Promise<WorkerTerminalStreamResult> {
	await ticket.granted;
	const rec = mgr.sessions.get(channelId);
	const resizeSeqAtEntry = mgr.channelResizeSeq.get(channelId) ?? 0;
	if (!rec) {
		ticket.release();
		return failed(state, resizeSeqAtEntry, "session_not_live", "session is not live");
	}
	if (mgr.terminalStreams.get(channelId) !== state) {
		ticket.release();
		return failed(state, resizeSeqAtEntry, "retryable_pre_write", "terminal stream was superseded before keeper admission");
	}
	if (budget && !budget.isCurrentConnection()) {
		ticket.release();
		return failed(state, resizeSeqAtEntry, "retryable_pre_write", "worker connection was superseded before keeper admission");
	}
	if (budget && budget.remainingMs() <= 0) {
		ticket.release();
		return failed(state, resizeSeqAtEntry, "retryable_pre_write", "terminal stream budget expired before keeper admission");
	}
	if (!state.enabled) {
		ticket.release();
		return committed(state, resizeSeqAtEntry, false);
	}
	if (!state.coreValid) {
		ticket.release();
		return failed(state, resizeSeqAtEntry, "core_failed", "terminal core is invalid and requires adoption");
	}

	const currentCols = rec.wtermCore.getCols();
	const currentRows = rec.wtermCore.getRows();
	if (currentCols === state.cols && currentRows === state.rows) {
		ticket.release();
		if (mgr.terminalStreams.get(channelId) === state) mgr.installTerminalBaseline(rec.channelId);
		if (!await awaitBaselineInstallation(state)) {
			return failed(state, resizeSeqAtEntry, "core_failed", "full baseline was not installed before the stream deadline", "ambiguous");
		}
		if (!state.coreValid) {
			return failed(state, resizeSeqAtEntry, "core_failed", "full baseline could not be encoded", "ambiguous");
		}
		return committed(state, resizeSeqAtEntry, false);
	}
	let command: KeeperCommand<KeeperResizeResult>;
	let capture: LiveResizeCapture;
	const resizeSeq = resizeSeqAtEntry + 1;
	try {
		if (!mgr.sessions.has(channelId)) {
			return failed(state, resizeSeqAtEntry, "session_not_live", "session closed before the keeper resize");
		}
		if (budget && (!budget.isCurrentConnection() || budget.remainingMs() <= 0)) {
			return failed(state, resizeSeqAtEntry, "retryable_pre_write", "terminal stream expired before the keeper resize");
		}
		capture = installLiveResizeCapture(
			mgr,
			channelId,
			state,
			resizeSeq,
			currentCols,
			currentRows,
			state.cols,
			state.rows,
		);
		command = getMultiplexedPool().beginResize(
			channelId,
			resizeSeq,
			state.cols,
			state.rows,
			(result) => applyResizeResultAtBoundary(mgr, channelId, capture!, result),
		);
		if (!command.admission.written) {
			applyResizeResultAtBoundary(mgr, channelId, capture, {
				kind: "reject",
				seq: resizeSeq,
				reason: "disconnected",
			});
			return failed(
				state,
				resizeSeqAtEntry,
				command.admission.reason === "invalid_request" ? "invalid_request" : "retryable_pre_write",
				`keeper did not admit terminal resize: ${command.admission.reason}`,
			);
		}
		mgr.channelResizeSeq.set(channelId, resizeSeq);
	} catch (error) {
		return failed(
			state,
			resizeSeqAtEntry,
			"ambiguous_boundary",
			error instanceof Error ? error.message : String(error),
			"ambiguous",
		);
	} finally {
		ticket.release();
	}

	let result = await command.result;
	if (result.kind === "unknown") {
		const status = getMultiplexedPool().beginResizeStatus(
			channelId,
			resizeSeq,
			(statusResult) => {
				if (statusResult.kind === "reject") {
					applyResizeResultAtBoundary(mgr, channelId, capture, statusResult);
				}
			},
		);
		if (status.admission.written) result = await status.result;
	}
	if (result.kind === "reject") {
		if (capture.failedReason) {
			return failed(state, resizeSeq, "core_failed", capture.failedReason, "ambiguous");
		}
		if (capture.boundaryApplied) {
			const failure = result.reason === "channel_missing" || result.reason === "channel_exited"
				? "session_not_live"
				: result.reason === "invalid_request"
					? "invalid_request"
					: "retryable_pre_write";
			return failed(
				state,
				resizeSeq,
				failure,
				`keeper rejected terminal resize: ${result.reason}`,
				"rejected",
				"written",
			);
		}
	}
	if (result.kind !== "ack" || !capture.boundaryApplied) {
		const recovery = await recoverAmbiguousResize(mgr, channelId, capture);
		if (!recovery.ok) {
			return failed(state, resizeSeq, "ambiguous_boundary", recovery.reason, "ambiguous");
		}
	}
	if (capture.failedReason || !mgr.terminalStreams.get(channelId)?.coreValid) {
		return failed(state, resizeSeq, "core_failed", capture.failedReason ?? "terminal core resize failed", "ambiguous");
	}
	mgr.lastAppliedSize.set(channelId, { cols: state.cols, rows: state.rows });
	if (mgr.terminalStreams.get(channelId) === state) {
		mgr.installTerminalBaseline(rec.channelId);
		if (!await awaitBaselineInstallation(state)) {
			return failed(state, resizeSeq, "core_failed", "full baseline was not installed before the stream deadline", "ambiguous");
		}
		if (!state.coreValid) {
			return failed(state, resizeSeq, "core_failed", "full baseline could not be encoded", "ambiguous");
		}
	}
	return committed(state, resizeSeq, true);
}
