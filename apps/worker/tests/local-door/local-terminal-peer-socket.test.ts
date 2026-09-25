// Focused direct-carrier hello tests. Peer ports must prove the exact offer tuple
// before LocalTerminalSockets marks native framing authenticated; loopback keeps
// its rolling compatibility path only when peer_id and worker_epoch are absent.

import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { afterEach, expect, test } from "bun:test";
import { createHash, randomBytes, randomUUID } from "node:crypto";
import {
	LocalTerminalClientFrameSchema,
	LocalTerminalServerFrameSchema,
	type LocalTerminalServerFrame,
} from "@roost/protocol/proto/local_terminal_pb";
import { DLocalTerminalGrantSchema } from "@roost/protocol/proto/worker_transport_pb";
import { InputCommandSchema } from "@roost/protocol/proto/sync_pb";
import { LocalTerminalGrantStore } from "../../src/local-door/local-terminal-grants.ts";
import { LocalTerminalSockets } from "../../src/local-door/local-terminal-socket.ts";
import { TerminalInputRouteOwner } from "../../src/terminal/terminal-input-route-owner.ts";
import { TerminalInputWorkBudget } from "../../src/terminal/terminal-input-work-budget.ts";
import type { TerminalPacketPort } from "../../src/terminal/peer/terminal-packet-port.ts";
import type { TerminalPeerExpectedTuple } from "../../src/terminal/peer/terminal-peer-connection.ts";
import { TerminalViewOwner } from "../../src/terminal/view/terminal-view-owner.ts";
import { installAutoKeeper } from "../keeper-fake-pool.ts";
import {
	cleanupStreamHarnesses,
	makeHarness,
	SESSION_ID,
	TEST_COLS,
	TEST_ROWS,
	trackKeeper,
} from "../terminal/terminal-stream-state-harness.ts";

const DEVICE = "a".repeat(64);
const TAB = "peer-tab";
const WORKER_FP = "b".repeat(64);
const WORKER_EPOCH = "11111111-1111-4111-8111-111111111111";

interface PeerFixture {
	sockets: LocalTerminalSockets;
	grantId: string;
	secret: string;
	dispose(): void;
}

const disposers: Array<() => void> = [];

afterEach(() => {
	for (const dispose of disposers.splice(0)) dispose();
	cleanupStreamHarnesses();
});

async function makePeerFixture(): Promise<PeerFixture> {
	trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
	const harness = await makeHarness();
	const owner = new TerminalViewOwner({
		sessions: () => harness.manager,
		sendViewState: () => undefined,
		sendProjection: () => undefined,
	});
	const grants = new LocalTerminalGrantStore({ workerEpoch: WORKER_EPOCH });
	const inputWorkBudget = new TerminalInputWorkBudget();
	const inputRouteOwner = new TerminalInputRouteOwner({
		workerEpoch: WORKER_EPOCH,
		sessions: () => harness.manager,
		inputWorkBudget,
	});
	const secret = randomBytes(32).toString("hex");
	const grantId = randomUUID();
	grants.install(create(DLocalTerminalGrantSchema, {
		requestId: randomUUID(),
		grantId,
		secretSha256: createHash("sha256").update(secret).digest("hex"),
		sessionIds: [String(SESSION_ID)],
		deviceFingerprint: DEVICE,
		tabId: TAB,
		ttlMs: 60_000,
		workerEpoch: WORKER_EPOCH,
	}));
	const sockets = new LocalTerminalSockets({
		sessions: () => harness.manager,
		grants,
		viewOwner: owner,
		inputWorkBudget,
		inputRouteOwner,
		workerFingerprint: WORKER_FP,
		workerEpoch: WORKER_EPOCH,
	});
	const dispose = () => {
		sockets.dispose();
		inputRouteOwner.dispose();
		inputWorkBudget.dispose();
		grants.dispose();
		owner.dispose();
	};
	disposers.push(dispose);
	return { sockets, grantId, secret, dispose };
}

function helloFrame(fixture: PeerFixture, peerId = "", workerEpoch = ""): Uint8Array {
	return toBinary(LocalTerminalClientFrameSchema, create(LocalTerminalClientFrameSchema, {
		frame: {
			case: "hello",
			value: {
				grantId: fixture.grantId,
				secret: fixture.secret,
				tabId: TAB,
				deviceFingerprint: DEVICE,
				peerId,
				workerEpoch,
			},
		},
	}));
}

test("peer hello matches the offer tuple before it becomes authenticated", async () => {
	const fixture = await makePeerFixture();
	const peerId = randomUUID();
	const expected: TerminalPeerExpectedTuple = {
		peerId,
		grantId: fixture.grantId,
		deviceFingerprint: DEVICE,
		tabId: TAB,
		workerEpoch: WORKER_EPOCH,
	};
	const frames: LocalTerminalServerFrame[] = [];
	let authenticated = false;
	let open = true;
	let ingress: { onMessage(bytes: Uint8Array): void; onClose(): void } | undefined;
	const port: TerminalPacketPort & { markAuthenticated(): void } = {
		socketId: randomUUID(),
		kind: "webrtc",
		get open(): boolean { return open; },
		bufferedBytes: () => 0,
		send: (bytes) => {
			frames.push(fromBinary(LocalTerminalServerFrameSchema, bytes));
			return "accepted";
		},
		close: () => {
			if (!open) return;
			open = false;
			ingress?.onClose();
		},
		markAuthenticated: () => { authenticated = true; },
	};
	ingress = fixture.sockets.openPeerPort(port, expected);
	ingress.onMessage(helloFrame(fixture, peerId, WORKER_EPOCH));

	expect(authenticated).toBe(true);
	expect(frames[0]?.frame).toMatchObject({
		case: "ready",
		value: {
			workerEpoch: WORKER_EPOCH,
			socketId: port.socketId,
			peerId,
		},
	});
	ingress.onMessage(toBinary(LocalTerminalClientFrameSchema, create(LocalTerminalClientFrameSchema, {
		frame: {
			case: "input",
			value: create(InputCommandSchema, {
				sessionId: String(SESSION_ID),
				inputSeq: 1n,
				data: new Uint8Array([120]),
				inputRouteEpoch: "",
			}),
		},
	})));
	for (let turn = 0; turn < 8; turn += 1) await Promise.resolve();
	expect(frames.at(-1)?.frame).toMatchObject({
		case: "inputRejected",
		value: { reason: "terminal input route changed" },
	});
});

test("loopback rejects a peer-shaped hello instead of accepting old compatibility", async () => {
	const fixture = await makePeerFixture();
	const frames: LocalTerminalServerFrame[] = [];
	let open = true;
	const port: TerminalPacketPort = {
		socketId: randomUUID(),
		kind: "loopback",
		get open(): boolean { return open; },
		bufferedBytes: () => 0,
		send: (bytes) => {
			frames.push(fromBinary(LocalTerminalServerFrameSchema, bytes));
			return "accepted";
		},
		close: () => { open = false; },
	};
	fixture.sockets.onOpen(port);
	fixture.sockets.onMessage(port, helloFrame(fixture, randomUUID(), WORKER_EPOCH));

	expect(open).toBe(false);
	expect(frames[0]?.frame).toMatchObject({
		case: "closed",
		value: { reason: "loopback hello must not include peer identity" },
	});
});
