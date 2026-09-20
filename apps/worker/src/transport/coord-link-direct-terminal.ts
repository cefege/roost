// Typed coordinator->worker direct-terminal controls outside the general frame
// switch. Peer answers/errors, route results, and probes are unsequenced controls;
// stale coordinator sockets never publish a result onto a replacement connection.
// SDP and native errors stay inside the peer owner and never enter a log here.

import { create } from "@bufbuild/protobuf";
import {
	WLocalTerminalPeerErrorSchema,
	type DLocalTerminalPeerCancel,
	type DLocalTerminalPeerOffer,
	type DTerminalDirectRetire,
	type DTerminalInputRouteClaim,
	type DTerminalTransportProbe,
} from "@roost/shared/proto/worker_transport_pb";
import { TerminalInputRouteResultSchema } from "@roost/shared/proto/sync_pb";
import { TerminalPeerOfferError, type TerminalPeerOfferFailureReason } from "../terminal-peer-owner.ts";
import type {
	CoordLinkDeps,
	CoordLinkOutbox,
	TerminalRequestBudget,
	UpstreamFrame,
} from "./coord-link-types.ts";

export interface CoordLinkTerminalBudgetFactory {
	(socket: WebSocket, budgetMs: number): TerminalRequestBudget;
}

export interface CoordLinkDirectTerminalDownstream {
	handle(frameCase: string, value: unknown, socket: WebSocket): boolean;
}

export function createCoordLinkDirectTerminalDownstream(
	deps: CoordLinkDeps,
	outbox: CoordLinkOutbox,
	send: (frame: UpstreamFrame) => boolean,
	terminalBudget: CoordLinkTerminalBudgetFactory,
): CoordLinkDirectTerminalDownstream {
	function isCurrent(socket: WebSocket): boolean {
		return outbox.activeSocket() === socket;
	}

	function sendPeerError(request: DLocalTerminalPeerOffer, reason: TerminalPeerOfferFailureReason): void {
		send({
			kind: "local-terminal-peer-error",
			error: create(WLocalTerminalPeerErrorSchema, {
				requestId: request.requestId,
				connectionGeneration: request.connectionGeneration,
				workerEpoch: deps.processEpoch,
				peerId: request.peerId,
				reason,
			}),
		});
	}

	function refusedClaim(request: DTerminalInputRouteClaim): void {
		send({
			kind: "terminal-input-route-result",
			request_id: request.requestId,
			result: create(TerminalInputRouteResultSchema, {
				requestId: request.requestId,
				sessionId: request.sessionId,
				revision: request.revision,
				accepted: false,
				latestRevision: 0n,
				inputRouteEpoch: "",
				workerEpoch: deps.processEpoch,
				reason: "route_claim_busy",
			}),
		});
	}

	function handle(frameCase: string, value: unknown, socket: WebSocket): boolean {
		switch (frameCase) {
			case "localTerminalPeerOffer": {
				const request = value as DLocalTerminalPeerOffer;
				if (!isCurrent(socket)) return true;
				if (!deps.onLocalTerminalPeerOffer) {
					if (isCurrent(socket)) sendPeerError(request, "disabled");
					return true;
				}
				void deps.onLocalTerminalPeerOffer(request, terminalBudget(socket, request.budgetMs))
					.then((answer) => {
						if (isCurrent(socket)) send({ kind: "local-terminal-peer-answer", answer });
					})
					.catch((error: unknown) => {
						if (!isCurrent(socket)) return;
						const reason = error instanceof TerminalPeerOfferError ? error.reason : "ice_failed";
						sendPeerError(request, reason);
					});
				return true;
			}
			case "localTerminalPeerCancel":
				if (isCurrent(socket)) deps.onLocalTerminalPeerCancel?.(value as DLocalTerminalPeerCancel);
				return true;
			case "terminalInputRouteClaim": {
				const request = value as DTerminalInputRouteClaim;
				if (!isCurrent(socket)) return true;
				if (!deps.onTerminalInputRouteClaim) {
					if (isCurrent(socket)) refusedClaim(request);
					return true;
				}
				void deps.onTerminalInputRouteClaim(request, terminalBudget(socket, request.budgetMs))
					.then((result) => {
						if (isCurrent(socket)) {
							send({
								kind: "terminal-input-route-result",
								request_id: request.requestId,
								result,
							});
						}
					})
					.catch(() => {
						if (isCurrent(socket)) refusedClaim(request);
					});
				return true;
			}
			case "terminalTransportProbe": {
				if (!isCurrent(socket)) return true;
				const result = deps.onTerminalTransportProbe?.(value as DTerminalTransportProbe);
				if (result) send({ kind: "terminal-transport-probe-result", result });
				return true;
			}
			case "terminalDirectRetire":
				if (isCurrent(socket)) deps.onTerminalDirectRetire?.(value as DTerminalDirectRetire);
				return true;
			default:
				return false;
		}
	}

	return { handle };
}
