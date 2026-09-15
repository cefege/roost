// Scrollback bridge for the loopback terminal socket: it wraps the ONE
// authoritative reader (browser-command-terminal.ts::readScrollbackCells) into
// the LocalScrollbackResponse the local page expects. The coordinator RPC wraps
// the same reader into rpc-ok, so history is never traversed twice. The grant's
// session set is checked here because this transport authorizes per grant.

import { create } from "@bufbuild/protobuf";
import { cellRowToProto } from "@roost/shared/cell/cell-proto";
import { ScrollbackHistoryFloor as PbScrollbackHistoryFloor } from "@roost/shared/proto/coordinator_pb";
import {
	LocalScrollbackResponseSchema,
	type LocalScrollbackRequest,
	type LocalScrollbackResponse,
} from "@roost/shared/proto/local_terminal_pb";
import type { ScrollbackHistoryFloor } from "@roost/shared/wire";
import { readScrollbackCells } from "./browser-command-terminal.ts";
import type { SessionManager } from "./session-manager.ts";

/** One total map, so a new floor reason cannot be silently dropped. */
const HISTORY_FLOOR_PROTO: Record<ScrollbackHistoryFloor, PbScrollbackHistoryFloor> = {
	none: PbScrollbackHistoryFloor.UNSPECIFIED,
	evicted: PbScrollbackHistoryFloor.EVICTED,
	resize_replay: PbScrollbackHistoryFloor.RESIZE_REPLAY,
};

export async function readLocalScrollback(
	sessions: SessionManager,
	request: LocalScrollbackRequest,
	allowsSession: (sessionId: string) => boolean,
): Promise<LocalScrollbackResponse> {
	if (!allowsSession(request.sessionId)) {
		return create(LocalScrollbackResponseSchema, {
			requestId: request.requestId,
			error: "terminal session is unavailable",
		});
	}
	// The reader works in the SPA's JSON-safe absolute row space; a row past it
	// could not have come from a frame this worker emitted.
	if (request.endRow > BigInt(Number.MAX_SAFE_INTEGER)) {
		return create(LocalScrollbackResponseSchema, {
			requestId: request.requestId,
			error: "scrollback end_row is out of range",
		});
	}
	const result = await readScrollbackCells(sessions, {
		sessionId: request.sessionId,
		gridEpoch: request.gridEpoch,
		endRow: Number(request.endRow),
		maxRows: request.maxRows,
	});
	if (!result.ok) {
		return create(LocalScrollbackResponseSchema, {
			requestId: request.requestId,
			error: result.error,
		});
	}
	const page = result.page;
	return create(LocalScrollbackResponseSchema, {
		requestId: request.requestId,
		rows: page.rows.map(cellRowToProto),
		cols: page.cols,
		scrollbackTotal: BigInt(page.total),
		startRow: BigInt(page.startRow),
		endRow: BigInt(page.endRow),
		gridEpoch: page.gridEpoch,
		historyFloor: HISTORY_FLOOR_PROTO[page.historyFloor],
	});
}
