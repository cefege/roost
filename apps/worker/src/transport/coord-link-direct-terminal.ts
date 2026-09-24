// Typed coordinator->worker direct-terminal controls outside the general frame
// switch. Peer answers/errors, route results, and probes are unsequenced controls;
// stale coordinator sockets never publish a result onto a replacement connection.
// SDP and native errors stay inside the peer owner and never enter a log here.

import { create } from "@bufbuild/protobuf";
import {
	WAttachmentDirectStatusSchema,
	WLocalAttachmentPeerErrorSchema,
	WLocalTerminalPeerErrorSchema,
	type DAttachmentDirectStatusRequest,
	type DLocalAttachmentPeerCancel,
	type DLocalAttachmentPeerOffer,
	type DLocalAttachmentGrant,
	type DLocalAttachmentGrantRevoke,
	type DLocalTerminalPeerCancel,
	type DLocalTerminalPeerOffer,
	type DTerminalDirectRetire,
	type DTerminalInputRouteClaim,
	type DTerminalTransportProbe,
} from "@roost/protocol/proto/worker_transport_pb";
import { TerminalInputRouteResultSchema } from "@roost/protocol/proto/sync_pb";
import { AttachmentTransferStatusSchema } from "@roost/protocol/proto/attachment_transfer_pb";
import { TerminalPeerOfferError, type TerminalPeerOfferFailureReason } from "../terminal-peer-owner.ts";
import { AttachmentPeerOfferError, type AttachmentPeerOfferFailureReason } from "../attachment-peer-owner.ts";
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
	function sendAttachmentPeerError(
		request: DLocalAttachmentPeerOffer,
		reason: AttachmentPeerOfferFailureReason,
	): void {
		send({
			kind: "local-attachment-peer-error",
			error: create(WLocalAttachmentPeerErrorSchema, {
				requestId: request.requestId,
				connectionGeneration: request.connectionGeneration,
				workerEpoch: deps.processEpoch,
				peerId: request.peerId,
				reason,
			}),
		});
	}

	function sendUnavailableAttachmentStatus(request: DAttachmentDirectStatusRequest): void {
		send({
			kind: "attachment-direct-status",
			status: create(WAttachmentDirectStatusSchema, {
				requestId: request.requestId,
				status: create(AttachmentTransferStatusSchema, {
					uploadId: request.uploadId,
					nextSeq: 0,
					bytesReceived: 0n,
					lastChunkSha256: "",
					committed: false,
					absPath: "",
					error: "upload_not_found",
				}),
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
			case "localAttachmentPeerOffer": {
				const request = value as DLocalAttachmentPeerOffer;
				if (!isCurrent(socket)) return true;
				if (!deps.onLocalAttachmentPeerOffer) {
					if (isCurrent(socket)) sendAttachmentPeerError(request, "disabled");
					return true;
				}
				void deps.onLocalAttachmentPeerOffer(request, terminalBudget(socket, request.budgetMs))
					.then((answer) => {
						if (isCurrent(socket)) send({ kind: "local-attachment-peer-answer", answer });
					})
					.catch((error: unknown) => {
						if (!isCurrent(socket)) return;
						const reason = error instanceof AttachmentPeerOfferError ? error.reason : "ice_failed";
						sendAttachmentPeerError(request, reason);
					});
				return true;
			}
			case "localAttachmentPeerCancel":
				if (isCurrent(socket)) deps.onLocalAttachmentPeerCancel?.(value as DLocalAttachmentPeerCancel);
				return true;
			case "attachmentDirectStatusRequest": {
				if (!isCurrent(socket)) return true;
				const request = value as DAttachmentDirectStatusRequest;
				const status = deps.onAttachmentDirectStatusRequest?.(request);
				if (status) send({ kind: "attachment-direct-status", status });
				else sendUnavailableAttachmentStatus(request);
				return true;
			}
			case "localAttachmentGrant": {
				const request = value as DLocalAttachmentGrant;
				if (!isCurrent(socket)) return true;
				if (!deps.onLocalAttachmentGrant) {
					send({
						kind: "rpc-error",
						request_id: request.requestId,
						message: "local attachment grants unsupported by this worker",
					});
					return true;
				}
				try {
					deps.onLocalAttachmentGrant(request);
					send({
						kind: "rpc-ok",
						request_id: request.requestId,
						data: { grant_id: request.grantId },
					});
				} catch (error) {
					send({
						kind: "rpc-error",
						request_id: request.requestId,
						message: error instanceof Error ? error.message : String(error),
					});
				}
				return true;
			}
			case "localAttachmentGrantRevoke":
				if (isCurrent(socket)) deps.onLocalAttachmentGrantRevoke?.(value as DLocalAttachmentGrantRevoke);
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
