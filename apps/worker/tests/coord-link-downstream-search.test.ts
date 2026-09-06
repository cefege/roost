// Coordinator-link search-frame validation at the worker transport boundary.
// Malformed browser JSON must fail through its authenticated outer correlation
// instead of dispatching or waiting for the coordinator timeout.

import { expect, test } from "bun:test";
import type { CoordWorkerDown } from "@roost/shared/proto/worker_transport_pb";
import { createCoordLinkDownstream } from "../src/transport/coord-link-downstream.ts";
import type {
	CoordLinkDeps,
	CoordLinkOutbox,
	UpstreamFrame,
} from "../src/transport/coord-link-types.ts";

test("invalid search JSON emits a correlated rpc-error", () => {
	const sent: UpstreamFrame[] = [];
	let dispatched = false;
	const socket = {} as WebSocket;
	const downstream = createCoordLinkDownstream({
		onBrowserCommand: () => { dispatched = true; },
	} as unknown as CoordLinkDeps, {
		send: (frame: UpstreamFrame) => { sent.push(frame); return true; },
		activeSocket: () => socket,
	} as unknown as CoordLinkOutbox);
	const invalid = {
		frame: {
			case: "browserCommand",
			value: {
				browserId: "browser",
				viewerId: "viewer",
				requestId: "correlated-request",
				frameJson: JSON.stringify({
					kind: "search-scrollback",
					request_id: "inner-request",
					session_id: "00000000-0000-0000-0000-000000000001",
					search_id: "search-id",
					grid_epoch: "",
					query: "🐙".repeat(257),
					case_sensitive: false,
					regex: false,
					max_rows: 4096,
					max_matches: 256,
				}),
			},
		},
	} as CoordWorkerDown;

	downstream.handleDownstream(invalid, false, socket);

	expect(dispatched).toBe(false);
	expect(sent).toEqual([{
		kind: "rpc-error",
		request_id: "correlated-request",
		message: "invalid browser command",
	}]);
});
