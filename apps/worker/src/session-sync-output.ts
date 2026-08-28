// Synchronized-output (DECSET 2026) hold machinery for the cell emitter.
// When an application opens a synchronized frame the emitter must withhold
// streaming frames, and an application that never closes the frame would
// otherwise withhold them forever — so every hold carries two independent
// ceilings (a wall timer and a pending-row count) whose firing IS the expiry.
// Called from session-emit's streaming path; state lives on SessionManager's
// per-channel maps.

import { scrollbackOrigin } from "@roost/shared/cell";
import type { TerminalCore } from "@wterm/core";
import { signal } from "@roost/shared/diag";
import { asChannelId } from "@roost/shared/wire";
import type { CellEmitState } from "@roost/shared/cell";
import type { SessionManager } from "./session-manager.ts";
import { SYNC_OUTPUT_MAX_MS, SYNC_OUTPUT_MAX_PENDING_ROWS } from "./session-constants.ts";
import { monoNowMs } from "./util/mono.ts";

/** One DEC 2026 synchronized-output frame whose intermediate cell sends the
 *  emitter is withholding. Bounded by a wall ceiling (the armed timer) and a
 *  pending-row ceiling; whichever trips first emits the withheld frame and
 *  stops this generation suppressing anything further. */
export interface SyncOutputHold {
	/** The core's synchronized-output generation this hold belongs to. */
	generation: number;
	/** Monotonic scrollback total when the hold opened; the work ceiling is
	 *  measured against it. */
	sbTotalAtOpen: number;
	/** Fires exactly at the wall ceiling. A producer that goes SILENT inside an
	 *  unterminated frame is the case only this timer can recover. */
	timer: NodeJS.Timeout | undefined;
	tripped: boolean;
}

/** What the streaming path must do with this chunk's frame. */
type SyncOutputAction =
	/** No synchronized frame is withholding anything: use the rate governor. */
	| "pass"
	/** Inside a synchronized frame with both ceilings intact: withhold. */
	| "hold"
	/** A boundary the browser is owed the withheld frame at: the application
	 *  closed a frame we withheld, or a ceiling just tripped. */
	| "flush";

export function syncOutputAction(mgr: SessionManager, channelId: number): SyncOutputAction {
	const rec = mgr.sessions.get(channelId);
	const core = rec?.wtermCore;
	if (!rec || !core) return "pass";
	const hold = mgr.syncOutputHolds.get(channelId);
	if (!(core.synchronizedOutput?.() ?? false)) {
		if (hold === undefined) return "pass";
		// The application closed its frame. A hold that actually withheld output
		// owes the browser one authoritative frame at the boundary the
		// application itself declared. A tripped hold normally already flushed,
		// except when that ceiling emission was withheld because it would have
		// resolved a pending full snapshot from the intermediate grid.
		const owed = !hold.tripped || mgr.pendingSyncCellSnapshots.has(channelId);
		releaseSyncOutputHold(mgr, channelId);
		return owed ? "flush" : "pass";
	}
	const generation = core.synchronizedOutputGeneration?.() ?? 0;
	if (hold !== undefined && hold.generation === generation) {
		if (hold.tripped) return "pass";
		if (syncPendingRows(core, rec.cell_emit, hold) < SYNC_OUTPUT_MAX_PENDING_ROWS) return "hold";
		tripSyncOutputHold(mgr, channelId, hold, "pending_rows");
		return "flush";
	}
	if (hold !== undefined && !hold.tripped) {
		// Closed and immediately reopened inside one chunk, with nothing emitted
		// in between: the browser has been continuously dark, so the new
		// generation INHERITS the running ceilings. Restarting them here is what
		// would let a `2026l 2026h` loop suppress forever one reset at a time.
		hold.generation = generation;
		return "hold";
	}
	if (hold !== undefined) releaseSyncOutputHold(mgr, channelId);
	mgr.syncOutputHolds.set(
		channelId,
		openSyncOutputHold(mgr, channelId, generation, monoScrollbackTotal(core, rec.cell_emit)),
	);
	return "hold";
}

/** Roost's monotonic scrollback total: the authoritative eviction origin plus
 *  everything the ring still holds. The origin is what keeps this monotonic at
 *  saturation, where getScrollbackCount() alone pins and stops moving. */
function monoScrollbackTotal(core: TerminalCore, emit: CellEmitState): number {
	return scrollbackOrigin(core, emit) + core.getScrollbackCount();
}

/** Rows the browser is missing inside this frame: history appended since the
 *  hold opened, plus the viewport rows currently dirty. Costs one WASM read per
 *  viewport row, and only on the suppressed path. */
function syncPendingRows(core: TerminalCore, emit: CellEmitState, hold: SyncOutputHold): number {
	const rows = core.getRows();
	let dirty = 0;
	for (let row = 0; row < rows; row++) if (core.isDirtyRow(row)) dirty++;
	return Math.max(0, monoScrollbackTotal(core, emit) - hold.sbTotalAtOpen) + dirty;
}

function openSyncOutputHold(
	mgr: SessionManager,
	channelId: number,
	generation: number,
	sbTotalAtOpen: number,
): SyncOutputHold {
	const hold: SyncOutputHold = { generation, sbTotalAtOpen, timer: undefined, tripped: false };
	// Armed for exactly the ceiling, so FIRING is the expiry: the decision never
	// re-reads a clock, and a host clock step can neither forge nor hide it.
	hold.timer = setTimeout(() => {
		hold.timer = undefined;
		if (mgr.syncOutputHolds.get(channelId) !== hold) return;
		// A deferred authoritative full must also bypass the baseline gate: its
		// stream is not baseline-ready by definition, so an unforced recovery
		// emit would be rejected before it could observe this tripped hold.
		// Deltas remain unforced so nextCellFrame preserves its normal choice.
		tripSyncOutputHold(mgr, channelId, hold, "elapsed_ms");
		const forceOwedFull = mgr.pendingSyncCellSnapshots.has(channelId);
		mgr.emitCellFrame(asChannelId(channelId), forceOwedFull);
	}, SYNC_OUTPUT_MAX_MS);
	return hold;
}

/** Stop withholding for this generation and leave the trip legible in the
 *  diagnostic snapshot: gate, age and suppressed-frame count stay put until the
 *  application finally closes its frame, because a terminal stuck inside an
 *  unterminated synchronized frame is exactly what an operator needs to see. */
function tripSyncOutputHold(
	mgr: SessionManager,
	channelId: number,
	hold: SyncOutputHold,
	cap: "elapsed_ms" | "pending_rows",
): void {
	hold.tripped = true;
	clearTimeout(hold.timer);
	hold.timer = undefined;
	const state = mgr.cellGateSuppression.get(channelId);
	if (state?.gate === "sync_output") state.overBudget = true;
	signal("terminal.sync_output_cap", {
		sid: String(mgr.sessions.get(channelId)?.sessionId ?? ""),
		channel_id: channelId,
		cap,
		generation: hold.generation,
		age_ms: state ? Math.round(monoNowMs() - state.sinceMonoMs) : 0,
		suppressed_frames: state?.frames ?? 0,
		cap_ms: SYNC_OUTPUT_MAX_MS,
		cap_rows: SYNC_OUTPUT_MAX_PENDING_ROWS,
		cooldownKey: String(channelId),
	});
}

/** Stop withholding and forget the generation entirely. Exported because the
 *  resize transaction has to retire a hold from OUTSIDE the streaming path: a
 *  hold's `generation` and `sbTotalAtOpen` are expressed in ONE core instance's
 *  terms, so both points where that instance stops being what the emitter reads
 *  — the capture that freezes it, and the swap that replaces it — must retire
 *  it. Left armed across either, its wall timer fires an emit into the resize
 *  gate, which emitCellFrame checks BEFORE the force bypass and therefore
 *  discards: the 1 s recovery ceiling is spent invisibly inside the transaction,
 *  and the next chunk then either inherits a dead core's generation or opens a
 *  fresh 1 s ceiling stacked on top of the resize's own budget. */
export function releaseSyncOutputHold(mgr: SessionManager, channelId: number): void {
	const hold = mgr.syncOutputHolds.get(channelId);
	if (hold === undefined) return;
	clearTimeout(hold.timer);
	mgr.syncOutputHolds.delete(channelId);
	if (mgr.cellGateSuppression.get(channelId)?.gate === "sync_output") {
		mgr.cellGateSuppression.delete(channelId);
	}
}
