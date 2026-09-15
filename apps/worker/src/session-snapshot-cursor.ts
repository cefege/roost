// Immutable full-snapshot cursors for cell emission: when an oversized delta
// or a forced baseline must ship as chunked parts, the frame is parked as a
// cancellable cursor PER SINK that the emitter drains part-by-part across
// subsequent emits. One built frame is validated and chunked once, then each
// active sink advances its own cursor and only on a "sent" result. Renewal
// epoch compatibility lives here because forced fulls bypass the emitter's
// semantic-reframe decision.
import {
	assertCellGridSnapshot,
	CELL_GRID_PART_MAX_BYTES,
	chunkCellGridFrame,
	encodedCellGridFrameSize,
	scrollbackOrigin,
	type CellEmitState,
} from "@roost/shared/cell";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import type { TerminalCore } from "@wterm/core";
import { signal } from "@roost/shared/diag";
import { randomUUID } from "node:crypto";
import {
	activeCellSinks,
	aggregateStreamDelivery,
	cellSinkDelivery,
	clearStreamDeliveryDirty,
	isCellSinkActive,
	sendCellPartToSink,
} from "./session-cell-sinks.ts";
import type { SessionManager } from "./session-manager.ts";
import type {
	TerminalSnapshotPart,
	TerminalStreamState,
} from "./session-terminal-state.ts";

/** Stop one sink's pending snapshot. Repeated retirement is harmless because
 * cursor ownership is one-shot. */
export function retireSnapshotCursor(
	mgr: SessionManager,
	channelId: number,
	state: TerminalStreamState,
	sinkId: string,
): void {
	const delivery = state.deliveries.get(sinkId);
	if (!delivery) return;
	// Read the map so callers pass the channel they are retiring, while keeping
	// the state object authoritative if it has already been superseded.
	const current = mgr.terminalStreams.get(channelId);
	if (current === state || delivery.cursor !== null || !delivery.baselineReady) {
		delivery.cursor = null;
		delivery.baselineReady = false;
	}
}

/** Stop the whole stream's delivery: stream replacement, teardown, resize trap
 * and explicit snapshot requests invalidate every sink's baseline at once. */
export function retireStreamDelivery(
	mgr: SessionManager,
	channelId: number,
	state: TerminalStreamState,
): void {
	for (const sinkId of state.deliveries.keys()) {
		retireSnapshotCursor(mgr, channelId, state, sinkId);
	}
}

/** Preserve a same-grid renewal epoch only while its absolute history range
 * still overlaps the prior canonical checkpoint. */
export function prepareCellRenewalEpoch(core: TerminalCore, emit: CellEmitState): void {
	const sbDropped = scrollbackOrigin(core, emit);
	const scrollbackTotal = sbDropped + core.getScrollbackCount();
	const compatible = core.getCols() === emit.cols
		&& core.getRows() === emit.rows
		&& core.usingAltScreen() === emit.alt
		&& sbDropped <= emit.lastSbTotal
		&& scrollbackTotal >= emit.lastSbTotal;
	if (!compatible) emit.gridEpochRevision++;
}

export function drainSnapshotCursor(
	mgr: SessionManager,
	channelId: number,
	state: TerminalStreamState,
	sinkId: string,
): void {
	const sink = mgr.cellSinks.get(sinkId)?.sink;
	if (!sink) return;
	let delivery = state.deliveries.get(sinkId);
	while (mgr.terminalStreams.get(channelId) === state && delivery?.cursor) {
		if (!isCellSinkActive(mgr, sinkId)) return;
		const cursor = delivery.cursor;
		const part = cursor.parts[cursor.nextPart];
		if (!part) return;
		const result = sendCellPartToSink(mgr, channelId, sink, part);
		// A sender may synchronously retire/replace this generation or drop the
		// sink. Never advance a cursor that no longer owns the channel, even if
		// the stale sender returned "sent".
		delivery = state.deliveries.get(sinkId);
		if (mgr.terminalStreams.get(channelId) !== state || delivery?.cursor !== cursor) return;
		// Cells are either on the wire or dropped for the existing writable
		// retry path. In particular, queue admission is not completion.
		if (result !== "sent") return;
		cursor.nextPart += 1;
		if (cursor.nextPart < cursor.parts.length) continue;
		delivery.cursor = null;
		delivery.baselineReady = true;
		completeStreamBaseline(mgr, channelId, state);
		return;
	}
}

/** Channel-wide effects fire only once the LAST active sink has a baseline:
 * until then a repair latch or gate release would let a delta reach a sink
 * that is still missing its full. */
function completeStreamBaseline(
	mgr: SessionManager,
	channelId: number,
	state: TerminalStreamState,
): void {
	const aggregate = aggregateStreamDelivery(mgr, state);
	if (aggregate.snapshotPending || !aggregate.baselineReady) return;
	mgr.pendingCellRepairs.delete(channelId);
	mgr.pendingSyncCellSnapshots.delete(channelId);
	if (!mgr.syncOutputHolds.has(channelId)) mgr.cellGateSuppression.delete(channelId);
	const dirty = aggregate.baselineDirty || mgr.cellDirty.has(channelId);
	clearStreamDeliveryDirty(state);
	if (dirty) mgr._scheduleCellEmit(channelId);
}

/** Park ONE built full as a cursor for every active sink, then drain them.
 * Validation and chunking run once: the immutable parts are shared, only each
 * sink's position in them is private. */
export function installStreamBaseline(
	mgr: SessionManager,
	channelId: number,
	state: TerminalStreamState,
	pb: PbCellGridFrame,
): boolean {
	const sinks = activeCellSinks(mgr);
	let parts: readonly TerminalSnapshotPart[];
	try {
		assertCellGridSnapshot(pb);
		const snapshotId = randomUUID();
		parts = encodedCellGridFrameSize(pb) <= CELL_GRID_PART_MAX_BYTES
			? [{ kind: "frame" as const, value: pb }]
			: chunkCellGridFrame(pb, snapshotId).map((value) => ({
				kind: "chunk" as const,
				value,
			}));
		for (const sink of sinks) {
			const delivery = cellSinkDelivery(state, sink.id);
			delivery.cursor = { streamId: state.streamId, snapshotId, seq: pb.seq, parts, nextPart: 0 };
			delivery.baselineReady = false;
		}
	} catch (error) {
		retireStreamDelivery(mgr, channelId, state);
		state.coreValid = false;
		signal("terminal.invalid_frame", {
			sid: String(mgr.sessions.get(channelId)?.sessionId ?? ""),
			channel_id: channelId,
			stream_id: state.streamId,
			reason: error instanceof Error ? error.message : String(error),
			cooldownKey: String(channelId),
		});
		return false;
	}
	// Every cursor is installed before the first part ships so an early
	// completion cannot report the stream baselined while a sibling still owes.
	for (const sink of sinks) drainSnapshotCursor(mgr, channelId, state, sink.id);
	return true;
}
