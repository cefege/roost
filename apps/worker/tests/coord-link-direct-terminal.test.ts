// Typed direct-terminal downstream coverage. These controls bypass generic JSON
// RPCs: peer offer/cancel, route claim/probe, and retirement retain generated
// worker frames and never dispatch from a superseded coordinator socket.

import { create } from "@bufbuild/protobuf";
import { expect, test } from "bun:test";
import {
	CoordWorkerDownSchema,
	DLocalTerminalPeerCancelSchema,
	DLocalTerminalPeerOfferSchema,
	DTerminalDirectRetireSchema,
	DTerminalInputRouteClaimSchema,
	DTerminalTransportProbeSchema,
	WLocalTerminalPeerAnswerSchema,
	WTerminalTransportProbeResultSchema,
} from "@roost/shared/proto/worker_transport_pb";
import { TerminalInputRouteResultSchema } from "@roost/shared/proto/sync_pb";
import { createCoordLinkDownstream } from "../src/transport/coord-link-downstream.ts";
import type {
	CoordLinkDeps,
	CoordLinkOutbox,
	UpstreamFrame,
} from "../src/transport/coord-link-types.ts";

const WORKER_EPOCH = "11111111-1111-4111-8111-111111111111";

async function settleControls(): Promise<void> {
	await Promise.resolve();
	await Promise.resolve();
}

test("direct downstream emits typed peer, route, and probe results while routing cancel and retirement", async () => {
	const sent: UpstreamFrame[] = [];
	const socket = {} as WebSocket;
	let cancelled = false;
	let retired = "";
	const deps: Partial<CoordLinkDeps> = {
		onLocalTerminalPeerOffer: async (request) => create(WLocalTerminalPeerAnswerSchema, {
			requestId: request.requestId,
			connectionGeneration: request.connectionGeneration,
			workerEpoch: WORKER_EPOCH,
			peerId: request.peerId,
			answerSdp: "answer",
		}),
		onLocalTerminalPeerCancel: () => { cancelled = true; },
		onTerminalInputRouteClaim: async (request) => create(TerminalInputRouteResultSchema, {
			requestId: request.requestId,
			sessionId: request.sessionId,
			revision: request.revision,
			accepted: true,
			latestRevision: request.revision,
			inputRouteEpoch: "route-epoch",
			workerEpoch: WORKER_EPOCH,
		}),
		onTerminalTransportProbe: (request) => create(WTerminalTransportProbeResultSchema, {
			requestId: request.requestId,
			workerEpoch: WORKER_EPOCH,
		}),
		onTerminalDirectRetire: (request) => { retired = request.reason; },
	};
	const downstream = createCoordLinkDownstream(deps as CoordLinkDeps, {
		send: (frame: UpstreamFrame) => {
			sent.push(frame);
			return true;
		},
		activeSocket: () => socket,
	} as CoordLinkOutbox);
	const peerId = "11111111-1111-4111-8111-111111111112";
	const connectionGeneration = "coord-generation";

	downstream.handleDownstream(create(CoordWorkerDownSchema, {
		frame: { case: "localTerminalPeerOffer", value: create(DLocalTerminalPeerOfferSchema, {
			requestId: "peer-offer",
			connectionGeneration,
			workerEpoch: WORKER_EPOCH,
			grantId: "grant-id",
			peerId,
			deviceFingerprint: "a".repeat(64),
			tabId: "tab-id",
			offerSdp: "offer",
			budgetMs: 8_000,
		}) },
	}), false, socket);
	downstream.handleDownstream(create(CoordWorkerDownSchema, {
		frame: { case: "terminalInputRouteClaim", value: create(DTerminalInputRouteClaimSchema, {
			requestId: "route-claim",
			sessionId: "22222222-2222-4222-8222-222222222222",
			deviceFingerprint: "a".repeat(64),
			tabId: "tab-id",
			browserConnectionId: "browser-connection",
			revision: 1n,
			budgetMs: 8_000,
			workerEpoch: WORKER_EPOCH,
		}) },
	}), false, socket);
	downstream.handleDownstream(create(CoordWorkerDownSchema, {
		frame: { case: "terminalTransportProbe", value: create(DTerminalTransportProbeSchema, {
			requestId: "probe",
			workerEpoch: WORKER_EPOCH,
		}) },
	}), false, socket);
	downstream.handleDownstream(create(CoordWorkerDownSchema, {
		frame: { case: "localTerminalPeerCancel", value: create(DLocalTerminalPeerCancelSchema, {
			requestId: "peer-offer",
			connectionGeneration,
			workerEpoch: WORKER_EPOCH,
			peerId,
		}) },
	}), false, socket);
	downstream.handleDownstream(create(CoordWorkerDownSchema, {
		frame: { case: "terminalDirectRetire", value: create(DTerminalDirectRetireSchema, {
			workerEpoch: WORKER_EPOCH,
			reason: "worker_deleted",
		}) },
	}), false, socket);
	await settleControls();

	expect(sent).toEqual(expect.arrayContaining([
		expect.objectContaining({ kind: "local-terminal-peer-answer" }),
		expect.objectContaining({ kind: "terminal-input-route-result", request_id: "route-claim" }),
		expect.objectContaining({ kind: "terminal-transport-probe-result" }),
	]));
	expect(cancelled).toBe(true);
	expect(retired).toBe("worker_deleted");
});
