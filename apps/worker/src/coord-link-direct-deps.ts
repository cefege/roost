// Direct terminal CoordLink callbacks composed into buildCoordLinkDeps. This file
// binds authenticated downstream controls to the process-owned grant, peer, route,
// and work owners without adding state to SessionManager or CoordLink itself.
// Peer connection generations are accepted once per attached coordinator socket.

import { create } from "@bufbuild/protobuf";
import {
	WTerminalTransportProbeResultSchema,
	type DLocalTerminalPeerCancel,
	type DLocalTerminalPeerOffer,
	type DTerminalDirectRetire,
	type DTerminalInputRouteClaim,
	type DTerminalTransportProbe,
	type WLocalTerminalPeerAnswer,
	type WTerminalTransportProbeResult,
} from "@roost/shared/proto/worker_transport_pb";
import {
	TerminalInputRouteClaimSchema,
	TerminalInputRouteResultSchema,
	type TerminalInputRouteResult,
} from "@roost/shared/proto/sync_pb";
import type { SessionManager } from "./session-manager.ts";
import { TerminalPeerOfferError } from "./terminal-peer-owner.ts";
import type { LocalTerminalWiring, TerminalRequestBudget } from "./transport/coord-link-types.ts";

export interface CoordLinkDirectDepsContext {
	readonly localTerminal: LocalTerminalWiring | undefined;
	readonly processEpoch: string;
	readonly sessions: () => SessionManager;
}

export interface CoordLinkDirectTerminalHandlers {
	onLocalTerminalPeerOffer(request: DLocalTerminalPeerOffer, budget: TerminalRequestBudget): Promise<WLocalTerminalPeerAnswer>;
	onLocalTerminalPeerCancel(request: DLocalTerminalPeerCancel): void;
	onTerminalInputRouteClaim(
		request: DTerminalInputRouteClaim,
		budget: TerminalRequestBudget,
	): Promise<TerminalInputRouteResult>;
	onTerminalTransportProbe(request: DTerminalTransportProbe): WTerminalTransportProbeResult | null;
	onTerminalDirectRetire(request: DTerminalDirectRetire): void;
}

export function makeCoordLinkDirectTerminalHandlers(
	ctx: CoordLinkDirectDepsContext,
): CoordLinkDirectTerminalHandlers {
	return {
		onLocalTerminalPeerOffer: async (request, budget) => {
			const local = ctx.localTerminal;
			if (!local || !local.useCoordinatorGeneration(request.connectionGeneration)) {
				throw new TerminalPeerOfferError("connection_superseded");
			}
			return await local.peerOwner.offer(request, budget);
		},
		onLocalTerminalPeerCancel: (request) => {
			const local = ctx.localTerminal;
			if (local?.useCoordinatorGeneration(request.connectionGeneration)) local.peerOwner.cancel(request);
		},
		onTerminalInputRouteClaim: async (request, budget) => {
			const local = ctx.localTerminal;
			if (!local) return rejectedRouteClaim(request, ctx.processEpoch, "route_claim_busy");
			return await local.inputRouteOwner.claim({
				deviceFingerprint: request.deviceFingerprint,
				tabId: request.tabId,
				connectionId: request.browserConnectionId,
			}, create(TerminalInputRouteClaimSchema, {
				requestId: request.requestId,
				sessionId: request.sessionId,
				revision: request.revision,
				domainGeneration: 0n,
				workerEpoch: request.workerEpoch,
			}), {
				...budget,
				isSessionAuthorized: () =>
					budget.isCurrentConnection()
					&& local.workerEpoch === ctx.processEpoch
					&& ctx.sessions().getBySessionId(request.sessionId) !== undefined,
			});
		},
		onTerminalTransportProbe: (request) => {
			if (!ctx.localTerminal || request.workerEpoch !== ctx.processEpoch) return null;
			return create(WTerminalTransportProbeResultSchema, {
				requestId: request.requestId,
				workerEpoch: ctx.processEpoch,
			});
		},
		onTerminalDirectRetire: (request) => {
			if (
				request.workerEpoch !== ctx.processEpoch
				|| (request.reason !== "worker_deleted" && request.reason !== "worker_revoked")
			) return;
			if (ctx.localTerminal?.terminalPeerTestFaults?.consumeDirectRetireDrop()) return;
			ctx.localTerminal?.retireDirect(request.reason);
		},
	};
}

function rejectedRouteClaim(
	request: DTerminalInputRouteClaim,
	workerEpoch: string,
	reason: "route_claim_busy",
): TerminalInputRouteResult {
	return create(TerminalInputRouteResultSchema, {
		requestId: request.requestId,
		sessionId: request.sessionId,
		revision: request.revision,
		accepted: false,
		latestRevision: 0n,
		inputRouteEpoch: "",
		workerEpoch,
		reason,
	});
}
