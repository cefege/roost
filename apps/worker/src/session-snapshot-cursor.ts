// Immutable full-snapshot cursors for cell emission: when an oversized delta
// or a forced baseline must ship as chunked parts, the frame is parked here as
// a cancellable cursor the emitter drains part-by-part across subsequent
// emits. Renewal epoch compatibility lives beside cursor installation because
// forced fulls bypass the emitter's semantic-reframe decision.
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
import { log } from "@roost/shared/log";
import { randomUUID } from "node:crypto";
import type { SessionManager } from "./session-manager.ts";
import type { TerminalStreamState } from "./session-terminal-state.ts";
import type { TerminalCellSendResult } from "./transport/coord-link-types.ts";


/** Stop a pending immutable snapshot. Repeated retirement is harmless because
 * cursor ownership is one-shot. */
export function retireSnapshotCursor(
	mgr: SessionManager,
	channelId: number,
	state: TerminalStreamState,
): void {
	// Read the map so callers pass the channel they are retiring, while keeping
	// the state object authoritative if it has already been superseded.
	const current = mgr.terminalStreams.get(channelId);
	if (current === state || state.snapshotCursor !== null || !state.baselineReady) {
		state.snapshotCursor = null;
		state.baselineReady = false;
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

function sendSnapshotPart(
	mgr: SessionManager,
	channelId: number,
	state: TerminalStreamState,
): TerminalCellSendResult {
	const cursor = state.snapshotCursor;
	if (!cursor || cursor.nextPart >= cursor.parts.length) return "dropped";
	const part = cursor.parts[cursor.nextPart]!;
	try {
		if (part.kind === "frame") {
			return mgr.sendCellGridUpstream?.(channelId, part.value) ?? "dropped";
		}
		return mgr.sendCellGridChunkUpstream?.(channelId, part.value) ?? "dropped";
	} catch (error) {
		log.warn("session-manager", "cell_sink_throw", {
			channelId,
			error: error instanceof Error ? error.message : String(error),
		});
		return "dropped";
	}
}

export function drainSnapshotCursor(
	mgr: SessionManager,
	channelId: number,
	state: TerminalStreamState,
): void {
	while (mgr.terminalStreams.get(channelId) === state && state.snapshotCursor) {
		const cursor = state.snapshotCursor;
		const result = sendSnapshotPart(mgr, channelId, state);
		// A sender may synchronously retire/replace this generation. Never
		// advance a cursor that no longer owns the channel, even if the stale
		// sender returned "sent".
		if (mgr.terminalStreams.get(channelId) !== state || state.snapshotCursor !== cursor) return;
		// Cells are either on the wire or dropped for the existing writable
		// retry path. In particular, queue admission is not completion.
		if (result !== "sent") return;
		cursor.nextPart += 1;
		if (cursor.nextPart < cursor.parts.length) continue;
		state.snapshotCursor = null;
		state.baselineReady = true;
		mgr.pendingCellRepairs.delete(channelId);
		mgr.pendingSyncCellSnapshots.delete(channelId);
		if (!mgr.syncOutputHolds.has(channelId)) mgr.cellGateSuppression.delete(channelId);
		const dirty = state.baselineDirty || mgr.cellDirty.has(channelId);
		state.baselineDirty = false;
		if (dirty) mgr._scheduleCellEmit(channelId);
	}
}

export function installSnapshotCursor(
	mgr: SessionManager,
	channelId: number,
	state: TerminalStreamState,
	pb: PbCellGridFrame,
): boolean {
	if (state.snapshotCursor) retireSnapshotCursor(mgr, channelId, state);
	state.baselineReady = false;
	try {
		assertCellGridSnapshot(pb);
		const snapshotId = randomUUID();
		const parts = encodedCellGridFrameSize(pb) <= CELL_GRID_PART_MAX_BYTES
			? [{ kind: "frame" as const, value: pb }]
			: chunkCellGridFrame(pb, snapshotId).map((value) => ({
				kind: "chunk" as const,
				value,
			}));
		state.snapshotCursor = {
			streamId: state.streamId,
			snapshotId,
			seq: pb.seq,
			parts,
			nextPart: 0,
		};
		state.baselineReady = false;
		drainSnapshotCursor(mgr, channelId, state);
		return true;
	} catch (error) {
		retireSnapshotCursor(mgr, channelId, state);
		state.coreValid = false;
		state.baselineReady = false;
		signal("terminal.invalid_frame", {
			sid: String(mgr.sessions.get(channelId)?.sessionId ?? ""),
			channel_id: channelId,
			stream_id: state.streamId,
			reason: error instanceof Error ? error.message : String(error),
			cooldownKey: String(channelId),
		});
		return false;
	}
}
