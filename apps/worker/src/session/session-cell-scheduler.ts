// Cell emission cadence and gate suppression for SessionManager terminal streams.
// The scheduler owns one identity-fenced record per channel so replaced streams
// cannot consume, cancel, or re-arm another generation's work. Frame construction
// stays in session-emit; resize, snapshots, and synchronized output own their wakes.
import { asChannelId } from "@roost/protocol/wire";
import type { SessionManager } from "./session-manager.ts";
import {
	aggregateStreamDelivery,
	markStreamDeliveryDirty,
} from "./session-cell-sinks.ts";
import {
	CELL_EMIT_COALESCE_MS,
	SYNC_OUTPUT_MAX_MS,
} from "./session-constants.ts";
import {
	CELL_GATE_BUDGET_MS,
	noteGateOverBudget,
} from "./session-resize-capture.ts";
import type { TerminalStreamState } from "./session-terminal-state.ts";
import { syncOutputAction } from "./session-sync-output.ts";
import { monoNowMs } from "../util/mono.ts";

export interface CellEmissionSchedule {
	readonly stream: TerminalStreamState;
	/** Null while this exact record owns a queued leading microtask. */
	timer: Timer | null;
}

/** Which gate is withholding cell frames for a channel, since when (monotonic),
 * and how many frames it has suppressed. A stalled emitter is then attributable
 * from the diagnostic snapshot alone instead of by correlating logs. */
export interface CellGateSuppression {
	gate: "resize_capture" | "baseline" | "sync_output";
	sinceMonoMs: number;
	frames: number;
	/** The gate outlived its own ceiling: a resize/repair gate past the keeper
	 * command budget (corruption), or a synchronized-output hold past its cap
	 * (the withheld frame shipped and the stuck generation is bypassed). */
	overBudget: boolean;
	/** The ceiling `overBudget` is measured against. Per gate, because the
	 * synchronized-output hold answers to its own cap, not the keeper's. */
	budgetMs: number;
}

export function noteCellGateSuppression(
	mgr: SessionManager,
	channelId: number,
	gate: CellGateSuppression["gate"],
): void {
	const now = monoNowMs();
	const budgetMs = gate === "sync_output" ? SYNC_OUTPUT_MAX_MS : CELL_GATE_BUDGET_MS;
	let state = mgr.cellGateSuppression.get(channelId);
	if (!state || state.gate !== gate) {
		state = { gate, sinceMonoMs: now, frames: 0, overBudget: false, budgetMs };
		mgr.cellGateSuppression.set(channelId, state);
	}
	state.frames++;
	const ageMs = now - state.sinceMonoMs;
	// Past the keeper's own per-command budget the gate is no longer explainable
	// by one in-flight command; that is corruption, not latency. A
	// synchronized-output hold trips on its armed timer instead — firing IS the
	// expiry, so it never depends on a chunk arriving to re-read the clock.
	if (gate === "sync_output" || state.overBudget || ageMs <= state.budgetMs) return;
	state.overBudget = true;
	noteGateOverBudget(mgr, channelId, ageMs);
}

/** Retire the exact channel schedule. A queued leading microtask verifies its
 * record identity before acting, so deleting this entry is sufficient. */
export function cancelCellEmission(mgr: SessionManager, channelId: number): void {
	const schedule = mgr.cellEmitSchedules.get(channelId);
	if (!schedule) return;
	clearTimeout(schedule.timer ?? undefined);
	mgr.cellEmitSchedules.delete(channelId);
}

function hasLiveCurrentStream(
	mgr: SessionManager,
	channelId: number,
	stream: TerminalStreamState,
): boolean {
	if (mgr.terminalStreams.get(channelId) !== stream) return false;
	if (!mgr.sessions.has(channelId)) return false;
	return stream.enabled && stream.coreValid;
}

function mayRearmCellEmission(
	mgr: SessionManager,
	channelId: number,
	stream: TerminalStreamState,
): boolean {
	if (!hasLiveCurrentStream(mgr, channelId, stream)) return false;
	const delivery = aggregateStreamDelivery(mgr, stream);
	if (delivery.activeSinks === 0 || !delivery.baselineReady || delivery.snapshotPending) {
		return false;
	}
	if (mgr.cellEmissionGates.has(channelId) || mgr.pendingCellRepairs.has(channelId)) return false;
	const syncOutputHold = mgr.syncOutputHolds.get(channelId);
	return syncOutputHold === undefined || syncOutputHold.tripped;
}

function armTrailingCooldown(
	mgr: SessionManager,
	channelId: number,
	schedule: CellEmissionSchedule,
): void {
	if (mgr.cellEmitSchedules.has(channelId)) return;
	if (!mayRearmCellEmission(mgr, channelId, schedule.stream)) return;
	const timer = setTimeout(() => {
		if (mgr.cellEmitSchedules.get(channelId) !== schedule) return;
		if (!hasLiveCurrentStream(mgr, channelId, schedule.stream)) {
			mgr.cellEmitSchedules.delete(channelId);
			return;
		}
		mgr.cellEmitSchedules.delete(channelId);
		if (!mgr.cellDirty.has(channelId)) return;
		if (!mayRearmCellEmission(mgr, channelId, schedule.stream)) return;
		mgr.emitCellFrame(channelId, false);
		armTrailingCooldown(mgr, channelId, schedule);
	}, CELL_EMIT_COALESCE_MS);
	schedule.timer = timer;
	mgr.cellEmitSchedules.set(channelId, schedule);
}

function emitLeadingCellFrame(
	mgr: SessionManager,
	channelId: number,
	schedule: CellEmissionSchedule,
): void {
	if (mgr.cellEmitSchedules.get(channelId) !== schedule) return;
	if (!hasLiveCurrentStream(mgr, channelId, schedule.stream)) {
		mgr.cellEmitSchedules.delete(channelId);
		return;
	}
	if (schedule.timer !== null) return;
	mgr.cellEmitSchedules.delete(channelId);
	if (!mayRearmCellEmission(mgr, channelId, schedule.stream)) return;
	mgr.emitCellFrame(channelId, false);
	armTrailingCooldown(mgr, channelId, schedule);
}

/** Take one queued input-echo promotion for a channel, if it holds any. The
 * count exists because a fast burst admits several keystrokes before the first
 * return chunk arrives: consuming membership instead would promote only the
 * first echo and make every later one wait out CELL_EMIT_COALESCE_MS. */
export function consumeInputEchoPromotion(
	mgr: SessionManager,
	channelId: number,
): boolean {
	const held = mgr.inputSensitiveChannels.get(channelId) ?? 0;
	if (held <= 0) return false;
	if (held === 1) mgr.inputSensitiveChannels.delete(channelId);
	else mgr.inputSensitiveChannels.set(channelId, held - 1);
	return true;
}

/** Rate governor: leading-edge cell emit plus trailing coalesce. A single
 * input-sensitive return chunk may replace an armed trailing timer with a fresh
 * leading microtask; that promoted echo begins a new cooldown. */
export function scheduleCellEmission(
	mgr: SessionManager,
	channelId: number,
	promoteInputEcho = false,
): void {
	const stream = mgr.terminalStreams.get(channelId);
	if (!stream?.enabled || !stream.coreValid) return;
	const delivery = aggregateStreamDelivery(mgr, stream);
	// A stream nobody is delivering to records nothing: the next resumed or
	// newly registered sink owes a forced full regardless of this tick.
	if (delivery.activeSinks === 0) return;
	if (!delivery.baselineReady || delivery.snapshotPending) {
		markStreamDeliveryDirty(mgr, stream);
		mgr.cellDirty.add(channelId);
		noteCellGateSuppression(mgr, channelId, "baseline");
		return;
	}
	if (mgr.cellEmissionGates.has(channelId)) {
		mgr.cellDirty.add(channelId);
		noteCellGateSuppression(mgr, channelId, "resize_capture");
		return;
	}
	if (mgr.pendingCellRepairs.delete(channelId)) {
		mgr.installTerminalBaseline(asChannelId(channelId));
		return;
	}
	switch (syncOutputAction(mgr, channelId)) {
		case "hold":
			mgr.cellDirty.add(channelId);
			noteCellGateSuppression(mgr, channelId, "sync_output");
			// A trailing cooldown armed before the synchronized frame opened must
			// not leak a frame through the hold; its ceiling owns the next wake.
			cancelCellEmission(mgr, channelId);
			return;
		case "flush":
			mgr.emitCellFrame(channelId, false);
			return;
		case "pass":
			break;
	}
	const pending = mgr.cellEmitSchedules.get(channelId);
	if (pending) {
		if (pending.stream !== stream) {
			cancelCellEmission(mgr, channelId);
		} else {
			mgr.cellDirty.add(channelId);
			if (!promoteInputEcho || pending.timer === null) return;
			cancelCellEmission(mgr, channelId);
		}
	}
	const schedule: CellEmissionSchedule = { stream, timer: null };
	mgr.cellEmitSchedules.set(channelId, schedule);
	queueMicrotask(() => emitLeadingCellFrame(mgr, channelId, schedule));
}
