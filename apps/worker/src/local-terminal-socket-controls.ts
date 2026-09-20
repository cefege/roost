// Authenticated direct-port controls outside LocalTerminalSockets' carrier lifecycle.
// This owner receives only live predicates and sender callbacks; it never keeps a
// grant scope or a port after a call returns. Input-route claims, probes, and
// history replies remain control/history-lane messages rather than cell traffic.

import { create } from "@bufbuild/protobuf";
import {
	LocalScrollbackResponseSchema,
	type LocalScrollbackRequest,
	type LocalScrollbackResponse,
	type LocalTerminalServerFrame,
} from "@roost/shared/proto/local_terminal_pb";
import {
	TerminalTransportProbeResultSchema,
	type TerminalInputRouteClaim,
	type TerminalTransportProbe,
} from "@roost/shared/proto/sync_pb";
import {
	TERMINAL_PEER_LOGICAL_FRAME_MAX_BYTES,
	TERMINAL_PEER_WORKER_APPLICATION_QUEUE_MAX_BYTES,
} from "@roost/shared/terminal-peer";
import { readLocalScrollback } from "./local-terminal-scrollback.ts";
import type { LocalTerminalPortSession } from "./local-terminal-socket-authority.ts";
import type { SessionManager } from "./session-manager.ts";
import type { TerminalInputRouteActor, TerminalInputRouteOwner } from "./terminal-input-route-owner.ts";
import type { TerminalPacketPort, TerminalPacketSendResult } from "./terminal-packet-port.ts";
import type { TerminalPeerHistoryReadReservation } from "./terminal-peer-history-reservation.ts";
import type { TerminalRequestBudget } from "./transport/coord-link-types.ts";

export interface LocalTerminalPortControlsDeps {
	readonly sessions: () => SessionManager;
	readonly inputRouteOwner: TerminalInputRouteOwner;
	readonly workerFingerprint: string;
	readonly workerEpoch: string;
	readonly actor: (session: LocalTerminalPortSession) => TerminalInputRouteActor | null;
	readonly requestBudget: (session: LocalTerminalPortSession) => TerminalRequestBudget;
	readonly isSessionAuthorized: (session: LocalTerminalPortSession, sessionId: string) => boolean;
	readonly sendControl: (session: LocalTerminalPortSession, frame: LocalTerminalServerFrame["frame"]) => void;
	readonly sendHistory: (
		session: LocalTerminalPortSession,
		response: LocalScrollbackResponse,
		closeOnRefusal: boolean,
	) => TerminalPacketSendResult;
	readonly close: (session: LocalTerminalPortSession, reason: string) => void;
	readonly testFaults?: LocalTerminalPortControlsTestFaults;
}

interface HistoryDrainPort extends TerminalPacketPort {
	waitForLaneDrain(lane: "history"): Promise<void>;
	reserveHistoryRead(bytes: number): TerminalPeerHistoryReadReservation | null;
}

export type LocalTerminalPeerHistoryFault = (
	session: LocalTerminalPortSession,
	request: LocalScrollbackRequest,
	response: LocalScrollbackResponse,
) => Promise<boolean>;

export interface LocalTerminalPortControlsTestFaults {
	readonly onPeerHistoryResponse?: LocalTerminalPeerHistoryFault;
}

export class LocalTerminalPortControls {
	private readonly historyReads = new Set<string>();
	private loopbackHistoryReservedBytes = 0;

	constructor(private readonly deps: LocalTerminalPortControlsDeps) {}

	retirePort(socketId: string): void {
		this.historyReads.delete(socketId);
	}

	async claim(session: LocalTerminalPortSession, command: TerminalInputRouteClaim): Promise<void> {
		const actor = this.deps.actor(session);
		if (!actor) return;
		const result = await this.deps.inputRouteOwner.claim(actor, command, {
			...this.deps.requestBudget(session),
			isSessionAuthorized: () => this.deps.isSessionAuthorized(session, command.sessionId),
		});
		this.deps.sendControl(session, { case: "inputRouteResult", value: result });
	}

	probe(session: LocalTerminalPortSession, request: TerminalTransportProbe): void {
		if (request.workerFp !== this.deps.workerFingerprint) return;
		this.deps.sendControl(session, {
			case: "transportProbeResult",
			value: create(TerminalTransportProbeResultSchema, {
				requestId: request.requestId,
				workerFp: this.deps.workerFingerprint,
				workerEpoch: this.deps.workerEpoch,
			}),
		});
	}

	async scrollback(session: LocalTerminalPortSession, request: LocalScrollbackRequest): Promise<void> {
		const drainPort = historyDrainPort(session.port);
		if (this.historyReads.has(session.port.socketId)) {
			this.deps.sendHistory(session, create(LocalScrollbackResponseSchema, {
				requestId: request.requestId,
				error: "scrollback request is already pending",
			}), true);
			return;
		}
		const historyReservation = drainPort?.reserveHistoryRead(
			TERMINAL_PEER_LOGICAL_FRAME_MAX_BYTES.history - 4 * 1024,
		) ?? null;
		if (drainPort && !historyReservation) {
			this.deps.sendHistory(session, create(LocalScrollbackResponseSchema, {
				requestId: request.requestId,
				error: "scrollback response exceeds direct transport limit",
			}), true);
			return;
		}
		let loopbackReserved = false;
		if (!drainPort) {
			const bytes = TERMINAL_PEER_LOGICAL_FRAME_MAX_BYTES.history;
			if (this.loopbackHistoryReservedBytes > TERMINAL_PEER_WORKER_APPLICATION_QUEUE_MAX_BYTES - bytes) {
				this.deps.sendHistory(session, create(LocalScrollbackResponseSchema, {
					requestId: request.requestId,
					error: "scrollback response exceeds direct transport limit",
				}), true);
				return;
			}
			this.loopbackHistoryReservedBytes += bytes;
			loopbackReserved = true;
		}
		this.historyReads.add(session.port.socketId);
		try {
			let response = await readLocalScrollback(
				this.deps.sessions(),
				request,
				(sessionId) => this.deps.isSessionAuthorized(session, sessionId),
			);
			if (session.expectedPeer && this.deps.testFaults?.onPeerHistoryResponse) {
				let deliver = false;
				try {
					deliver = await this.deps.testFaults.onPeerHistoryResponse(session, request, response);
				} catch {
					// A smoke callback failure must not admit history across its test boundary.
				}
				if (!deliver) return;
			}
			if (!this.deps.isSessionAuthorized(session, request.sessionId)) {
				response = create(LocalScrollbackResponseSchema, {
					requestId: request.requestId,
					error: "terminal session is unavailable",
				});
			}
			historyReservation?.transfer();
			const delivered = this.deps.sendHistory(session, response, false);
			if (delivered !== "refused") {
				if (drainPort) await drainPort.waitForLaneDrain("history").catch(() => undefined);
				return;
			}
			if (response.error !== "") {
				this.deps.close(session, "local history delivery refused");
				return;
			}
			const fallback = this.deps.sendHistory(session, create(LocalScrollbackResponseSchema, {
				requestId: request.requestId,
				error: "scrollback response exceeds direct transport limit",
			}), true);
			if (fallback !== "refused" && drainPort) {
				await drainPort.waitForLaneDrain("history").catch(() => undefined);
			}
		} finally {
			historyReservation?.release();
			if (loopbackReserved) {
				this.loopbackHistoryReservedBytes -= TERMINAL_PEER_LOGICAL_FRAME_MAX_BYTES.history;
			}
			this.historyReads.delete(session.port.socketId);
		}
	}
}

function historyDrainPort(port: TerminalPacketPort): HistoryDrainPort | null {
	if (
		port.kind !== "webrtc"
		|| !("waitForLaneDrain" in port)
		|| typeof port.waitForLaneDrain !== "function"
	) return null;
	return port as HistoryDrainPort;
}
