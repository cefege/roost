// Registered cell-delivery sinks for SessionManager. The coordinator link and
// every local terminal socket receive the SAME emitted frame independently;
// this module owns registration, suspend/resume, the per-sink delivery records
// and the one aggregation every whole-stream delivery question goes through.
// Frame construction stays in session-emit.ts. A sink answering "overflow" is
// unregistered here and told once through onOverflow() to close its transport,
// so no local delivery queue can grow without bound.

import { log } from "@roost/shared/log";
import type { PbCellGridChunk, PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import { asChannelId } from "@roost/shared/wire";
import type { SessionManager } from "./session-manager.ts";
import type {
	TerminalSnapshotPart,
	TerminalStreamDelivery,
	TerminalStreamState,
} from "./session-terminal-state.ts";
import type { TerminalCellSendResult } from "./transport/coord-link-types.ts";

/** The coordinator link's sink. Local sockets register as `local:${socketId}`. */
export const COORD_CELL_SINK_ID = "coord";

/** A sink's answer for one frame. "overflow" is terminal: the registry drops
 * the sink instead of latching a repair for a queue that cannot drain. */
export type CellSinkResult = TerminalCellSendResult | "overflow";

export interface CellSink {
	readonly id: string;
	sendFrame(channelId: number, frame: PbCellGridFrame): CellSinkResult;
	sendChunk(channelId: number, chunk: PbCellGridChunk): CellSinkResult;
	/** Called once when the registry drops this sink for a delivery overflow, so
	 * its owner can close the transport. Never called by unregisterCellSink. */
	onOverflow?(): void;
}

export interface CellSinkRegistration {
	readonly sink: CellSink;
	/** A suspended sink is skipped entirely: no delivery, no bookkeeping. */
	active: boolean;
}

/** Every whole-stream delivery question, answered once over the ACTIVE sinks.
 * Suspended and unregistered sinks contribute nothing. */
export interface StreamDeliveryAggregate {
	activeSinks: number;
	/** Every active sink holds a complete baseline: deltas may flow. */
	baselineReady: boolean;
	/** An active sink is mid-snapshot, so the shared builder must not advance. */
	snapshotPending: boolean;
	/** Work arrived while an active sink's baseline was blocked. */
	baselineDirty: boolean;
	/** Snapshot parts still owed across active sinks. */
	remainingSnapshotParts: number;
	/** Parts in the snapshots currently in flight across active sinks. */
	snapshotPartCount: number;
}

export function registerCellSink(mgr: SessionManager, sink: CellSink): void {
	if (mgr.cellSinks.has(sink.id)) dropCellSink(mgr, sink.id);
	mgr.cellSinks.set(sink.id, { sink, active: true });
	log.info("session-manager", "cell_sink_registered", { sinkId: sink.id });
	forceBaselineForSink(mgr, sink.id);
}

export function unregisterCellSink(mgr: SessionManager, sinkId: string): void {
	if (!mgr.cellSinks.has(sinkId)) return;
	dropCellSink(mgr, sinkId);
	log.info("session-manager", "cell_sink_unregistered", { sinkId });
}

/** Transport known-down: stop delivering to this sink without latching a
 * repair or forcing a full, so a dead coordinator can never restart baselines
 * while another sink keeps painting. */
export function suspendCellSink(mgr: SessionManager, sinkId: string): void {
	const registration = mgr.cellSinks.get(sinkId);
	if (!registration?.active) return;
	registration.active = false;
	for (const state of mgr.terminalStreams.values()) {
		const delivery = state.deliveries.get(sinkId);
		if (!delivery) continue;
		delivery.cursor = null;
		delivery.baselineReady = false;
		delivery.baselineDirty = false;
	}
	log.info("session-manager", "cell_sink_suspended", { sinkId });
}

/** Transport usable again. A sink that was suspended owes a fresh baseline on
 * every watched channel; an already-active sink only resumes parked parts. */
export function resumeCellSink(mgr: SessionManager, sinkId: string): void {
	const registration = mgr.cellSinks.get(sinkId);
	if (!registration) return;
	if (registration.active) {
		mgr.resumeTerminalSnapshots();
		return;
	}
	registration.active = true;
	log.info("session-manager", "cell_sink_resumed", { sinkId });
	forceBaselineForSink(mgr, sinkId);
}

/** Sinks that must receive this tick's frame, snapshotted because a send may
 * synchronously unregister a sink or replace the stream. */
export function activeCellSinks(mgr: SessionManager): readonly CellSink[] {
	const sinks: CellSink[] = [];
	for (const registration of mgr.cellSinks.values()) {
		if (registration.active) sinks.push(registration.sink);
	}
	return sinks;
}

export function isCellSinkActive(mgr: SessionManager, sinkId: string): boolean {
	return mgr.cellSinks.get(sinkId)?.active === true;
}

/** This sink's delivery record for the stream, created on first delivery. A
 * missing record means the sink still owes a baseline. */
export function cellSinkDelivery(
	state: TerminalStreamState,
	sinkId: string,
): TerminalStreamDelivery {
	const existing = state.deliveries.get(sinkId);
	if (existing) return existing;
	const delivery: TerminalStreamDelivery = {
		cursor: null,
		baselineReady: false,
		baselineDirty: false,
	};
	state.deliveries.set(sinkId, delivery);
	return delivery;
}

/** A missing stream simply has no delivery records: every active sink owes a
 * baseline for it. */
export function aggregateStreamDelivery(
	mgr: SessionManager,
	state: TerminalStreamState | undefined,
): StreamDeliveryAggregate {
	const aggregate: StreamDeliveryAggregate = {
		activeSinks: 0,
		baselineReady: true,
		snapshotPending: false,
		baselineDirty: false,
		remainingSnapshotParts: 0,
		snapshotPartCount: 0,
	};
	for (const registration of mgr.cellSinks.values()) {
		if (!registration.active) continue;
		aggregate.activeSinks += 1;
		const delivery = state?.deliveries.get(registration.sink.id);
		if (!delivery?.baselineReady) aggregate.baselineReady = false;
		if (delivery?.baselineDirty) aggregate.baselineDirty = true;
		const cursor = delivery?.cursor;
		if (!cursor) continue;
		aggregate.snapshotPending = true;
		aggregate.snapshotPartCount += cursor.parts.length;
		aggregate.remainingSnapshotParts += Math.max(0, cursor.parts.length - cursor.nextPart);
	}
	return aggregate;
}

/** Dirty work observed while a baseline was blocked. Emission is stream-wide
 * over one core, so every active sink carries the same debt. */
export function markStreamDeliveryDirty(
	mgr: SessionManager,
	state: TerminalStreamState,
): void {
	for (const registration of mgr.cellSinks.values()) {
		if (registration.active) cellSinkDelivery(state, registration.sink.id).baselineDirty = true;
	}
}

/** Every active sink owes a fresh baseline: a dropped delta means at least one
 * receiver can no longer reproduce the shipped screen. */
export function invalidateStreamBaselines(
	mgr: SessionManager,
	state: TerminalStreamState,
): void {
	for (const registration of mgr.cellSinks.values()) {
		if (registration.active) cellSinkDelivery(state, registration.sink.id).baselineReady = false;
	}
}

export function clearStreamDeliveryDirty(state: TerminalStreamState): void {
	for (const delivery of state.deliveries.values()) delivery.baselineDirty = false;
}

export interface CellDeltaFanout {
	accepted: number;
	dropped: number;
}

/** Ship ONE built delta to every active sink. A "dropped" from an active sink
 * is the caller's repair signal; an overflow drops that sink alone. */
export function sendCellDeltaToSinks(
	mgr: SessionManager,
	channelId: number,
	frame: PbCellGridFrame,
): CellDeltaFanout {
	const fanout: CellDeltaFanout = { accepted: 0, dropped: 0 };
	for (const sink of activeCellSinks(mgr)) {
		// A sender may synchronously drop a later sink in this same snapshot.
		if (!isCellSinkActive(mgr, sink.id)) continue;
		const result = sendToCellSink(mgr, channelId, sink, () => sink.sendFrame(channelId, frame));
		if (result === "sent") fanout.accepted += 1;
		else if (result === "dropped") fanout.dropped += 1;
	}
	return fanout;
}

/** One snapshot part for one sink. An overflow already dropped the sink, so
 * the caller sees a non-advancing result and abandons that cursor. */
export function sendCellPartToSink(
	mgr: SessionManager,
	channelId: number,
	sink: CellSink,
	part: TerminalSnapshotPart,
): TerminalCellSendResult {
	const result = sendToCellSink(mgr, channelId, sink, () =>
		part.kind === "frame"
			? sink.sendFrame(channelId, part.value)
			: sink.sendChunk(channelId, part.value));
	return result === "sent" ? "sent" : "dropped";
}

function sendToCellSink(
	mgr: SessionManager,
	channelId: number,
	sink: CellSink,
	send: () => CellSinkResult,
): CellSinkResult {
	let result: CellSinkResult;
	try {
		result = send();
	} catch (error) {
		log.warn("session-manager", "cell_sink_throw", {
			sinkId: sink.id,
			channelId,
			error: error instanceof Error ? error.message : String(error),
		});
		return "dropped";
	}
	if (result !== "overflow") return result;
	dropCellSink(mgr, sink.id);
	log.warn("session-manager", "cell_sink_overflow", { sinkId: sink.id, channelId });
	sink.onOverflow?.();
	return "overflow";
}

function dropCellSink(mgr: SessionManager, sinkId: string): void {
	mgr.cellSinks.delete(sinkId);
	for (const state of mgr.terminalStreams.values()) state.deliveries.delete(sinkId);
}

/** One forced full per watched channel for a sink that just gained delivery.
 * The full is stream-wide because a second frame builder over one core would
 * steal the dirty rows the first frame already claimed. */
function forceBaselineForSink(mgr: SessionManager, sinkId: string): void {
	for (const [channelId, state] of mgr.terminalStreams) {
		if (!state.enabled || !state.coreValid || !mgr.sessions.has(channelId)) continue;
		const delivery = cellSinkDelivery(state, sinkId);
		delivery.baselineReady = false;
		// A cursor parked on ANY sink withholds the forced build, and the new
		// full supersedes it anyway. Leaving one parked here is what would darken
		// the channel for every sink: nothing else would ever force a full again.
		for (const parked of state.deliveries.values()) parked.cursor = null;
		mgr.installTerminalBaseline(asChannelId(channelId));
	}
}
