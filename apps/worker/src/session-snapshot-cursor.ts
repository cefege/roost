// Immutable full-snapshot cursors for cell emission: when an oversized delta
// or a forced baseline must ship as chunked parts, the frame is parked here as
// a cancellable cursor the emitter drains part-by-part across subsequent
// emits. Also validates renewal-history snapshots so a stale grid epoch can
// never pass as a full frame. Called only from session-emit.ts.
import {
	assertCellGridSnapshot,
	CELL_GRID_PART_MAX_BYTES,
	chunkCellGridFrame,
	encodedCellGridFrameSize,
	SB_RENEWAL_HISTORY_ROWS,
	SB_SNAPSHOT_HISTORY_ROWS,
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
import type { TransportSendResult } from "./transport/coord-link-types.ts";

/** Select the bounded history tail for a same-grid renewal full. An
 * incompatible renewal advances the epoch so a viewport-only full cannot be
 * mistaken for a continuation of the prior grid. */
export function renewalHistoryRows(core: TerminalCore, emit: CellEmitState): number {
	const sbDropped = scrollbackOrigin(core, emit);
	const scrollbackTotal = sbDropped + core.getScrollbackCount();
	const compatible = core.getCols() === emit.cols
		&& core.getRows() === emit.rows
		&& core.usingAltScreen() === emit.alt
		&& sbDropped <= emit.lastSbTotal
		&& scrollbackTotal >= emit.lastSbTotal;
	if (compatible) return SB_RENEWAL_HISTORY_ROWS;
	emit.gridEpochRevision++;
	return SB_SNAPSHOT_HISTORY_ROWS;
}

/** Exercise the same snapshot and chunk bounds as cursor installation before
 * committing renewal history to the emitter state. */
export function validateRenewalHistorySnapshot(pb: PbCellGridFrame): boolean {
	try {
		assertCellGridSnapshot(pb);
		if (encodedCellGridFrameSize(pb) > CELL_GRID_PART_MAX_BYTES) {
			chunkCellGridFrame(pb, randomUUID());
		}
		return true;
	} catch {
		return false;
	}
}

function sendSnapshotPart(
	mgr: SessionManager,
	channelId: number,
	state: TerminalStreamState,
): TransportSendResult {
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
		if (result === "dropped") return;
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
		state.resolveBaselineInstalled();
		drainSnapshotCursor(mgr, channelId, state);
		return true;
	} catch (error) {
		state.coreValid = false;
		state.snapshotCursor = null;
		state.resolveBaselineInstalled();
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
