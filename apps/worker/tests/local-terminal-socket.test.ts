// Proves what a browser on this machine may do with the worker's loopback
// terminal socket: only a hello that matches a coordinator-installed grant is
// admitted, membership and input are confined to that grant's sessions, input
// takes the real write path, a device revocation ends the socket, and a socket
// that stops draining is dropped alone. The real grant store, view owner,
// session manager, cell sinks and keeper socket run; only the WebSocket is a stub.

import { create, fromBinary, toBinary, type MessageInitShape } from "@bufbuild/protobuf";
import { afterEach, describe, expect, test } from "bun:test";
import { createHash, randomBytes, randomUUID } from "node:crypto";
import {
	PbCellGridFrameSchema,
	PbCellRowSchema,
	PbCellSpanSchema,
} from "@roost/shared/proto/cell_pb";
import {
	LocalTerminalClientFrameSchema,
	LocalTerminalServerFrameSchema,
	type LocalTerminalReady,
	type LocalTerminalServerFrame,
} from "@roost/shared/proto/local_terminal_pb";
import {
	InputCommandSchema,
	TerminalViewCommandSchema,
	TerminalViewStatus,
} from "@roost/shared/proto/sync_pb";
import { DLocalTerminalGrantSchema } from "@roost/shared/proto/worker_transport_pb";
import { LocalTerminalGrantStore } from "../src/local-terminal-grants.ts";
import { LocalTerminalSockets } from "../src/local-terminal-socket.ts";
import type { LocalTerminalSocket } from "../src/local-ui-server.ts";
import { sendCellDeltaToSinks } from "../src/session-cell-sinks.ts";
import { TerminalViewOwner } from "../src/terminal-view-owner.ts";
import { installAutoKeeper } from "./keeper-fake-pool.ts";
import {
	CHANNEL_ID,
	cleanupStreamHarnesses,
	makeHarness,
	SESSION_ID,
	TEST_COLS,
	TEST_ROWS,
	trackKeeper,
	type StreamHarness,
} from "./terminal-stream-state-harness.ts";

const DEVICE = "c".repeat(64);
const TAB = "tab-local";
const WORKER_FP = "d".repeat(64);
/** In the grant, but no live session: proves input consults the real write
 *  path instead of answering from the grant alone. */
const GRANTED_DEAD_SESSION = "22222222-3333-4333-8444-555555555555";
const UNGRANTED_SESSION = "33333333-4444-4333-8444-555555555555";

interface StubSocket {
	socket: LocalTerminalSocket;
	frames: LocalTerminalServerFrame[];
	closeReasons: string[];
	/** Bytes reported by send(); -1 is Bun's "enqueued under backpressure". */
	sendResult: number | null;
}

interface SocketFixture {
	harness: StreamHarness;
	owner: TerminalViewOwner;
	grants: LocalTerminalGrantStore;
	sockets: LocalTerminalSockets;
	secret: string;
	grantId: string;
	open(): StubSocket;
}

const owners: TerminalViewOwner[] = [];

afterEach(() => {
	for (const owner of owners.splice(0)) owner.dispose();
	cleanupStreamHarnesses();
});

async function makeFixture(): Promise<SocketFixture> {
	trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
	const harness = await makeHarness();
	const owner = new TerminalViewOwner({
		sessions: () => harness.manager,
		sendViewState: () => undefined,
		sendProjection: () => undefined,
	});
	owners.push(owner);
	const grants = new LocalTerminalGrantStore();
	const secret = randomBytes(32).toString("hex");
	const grantId = randomUUID();
	grants.install(create(DLocalTerminalGrantSchema, {
		requestId: randomUUID(),
		grantId,
		secretSha256: createHash("sha256").update(secret).digest("hex"),
		sessionIds: [String(SESSION_ID), GRANTED_DEAD_SESSION],
		deviceFingerprint: DEVICE,
		tabId: TAB,
		ttlMs: 60_000,
	}));
	const sockets = new LocalTerminalSockets({
		sessions: () => harness.manager,
		grants,
		viewOwner: owner,
		workerFingerprint: WORKER_FP,
	});
	return {
		harness,
		owner,
		grants,
		sockets,
		secret,
		grantId,
		open: () => {
			let live = true;
			const stub: StubSocket = {
				frames: [],
				closeReasons: [],
				sendResult: null,
				socket: {
					socketId: randomUUID(),
					send: (bytes: Uint8Array) => {
						stub.frames.push(fromBinary(LocalTerminalServerFrameSchema, bytes));
						return stub.sendResult ?? bytes.byteLength;
					},
					close: (_code?: number, reason?: string) => {
						stub.closeReasons.push(reason ?? "");
						if (!live) return;
						live = false;
						// Mirror the listener: a close always drives onClose.
						sockets.onClose(stub.socket);
					},
					get open(): boolean {
						return live;
					},
				},
			};
			sockets.onOpen(stub.socket);
			return stub;
		},
	};
}

type ClientFrameInit = MessageInitShape<typeof LocalTerminalClientFrameSchema>["frame"];

function sendClient(fixture: SocketFixture, stub: StubSocket, frame: ClientFrameInit): void {
	fixture.sockets.onMessage(
		stub.socket,
		toBinary(
			LocalTerminalClientFrameSchema,
			create(LocalTerminalClientFrameSchema, { frame }),
		),
	);
}

function hello(fixture: SocketFixture, secret: string) {
	return {
		case: "hello" as const,
		value: {
			grantId: fixture.grantId,
			secret,
			tabId: TAB,
			deviceFingerprint: DEVICE,
		},
	};
}

function view(sessionId: string, cols = 40, rows = 12) {
	return {
		case: "terminalView" as const,
		value: create(TerminalViewCommandSchema, {
			viewId: randomUUID(),
			sessionId,
			cols,
			rows,
			revision: 1n,
			active: true,
		}),
	};
}

function input(sessionId: string, seq: bigint, data: string) {
	return {
		case: "input" as const,
		value: create(InputCommandSchema, {
			sessionId,
			inputSeq: seq,
			data: new TextEncoder().encode(data),
		}),
	};
}

function frameCases(stub: StubSocket): string[] {
	return stub.frames.map((frame) => frame.frame.case ?? "unset");
}

/** One frame large enough that two of them exceed the socket's backpressure
 *  bound, so the overflow is reached without shipping thousands of frames. */
function oversizedDelta() {
	return create(PbCellGridFrameSchema, {
		sessionId: String(SESSION_ID),
		streamId: randomUUID(),
		seq: 2n,
		baseSeq: 1n,
		full: false,
		viewportRows: [create(PbCellRowSchema, {
			index: 0,
			spans: [create(PbCellSpanSchema, { text: "x".repeat(3_000_000) })],
		})],
	});
}

/** The oneof case is the narrowing, so a frame read never asserts a shape. */
function readyFrame(stub: StubSocket): LocalTerminalReady {
	const frame = stub.frames[0]?.frame;
	if (frame?.case !== "ready") {
		throw new Error(`expected a ready frame, got ${frame?.case ?? "none"}`);
	}
	return frame.value;
}

/** Awaits the frame the socket produces rather than a duration; the keeper
 *  fixture answers on microtasks, so a bounded drain is deterministic. */
async function frameArrives(stub: StubSocket, kind: string): Promise<void> {
	for (let turn = 0; turn < 1_000; turn += 1) {
		if (frameCases(stub).includes(kind)) return;
		await Promise.resolve();
	}
	throw new Error(`no ${kind} frame arrived; got ${frameCases(stub).join(",")}`);
}

describe("local terminal socket", () => {
	test("a hello whose secret does not match is closed and registers nothing", async () => {
		const fixture = await makeFixture();
		const stub = fixture.open();

		sendClient(fixture, stub, hello(fixture, "not-the-secret"));

		expect(frameCases(stub)).toEqual(["closed"]);
		expect(stub.frames[0]!.frame.value).toMatchObject({
			reason: "local terminal grant secret mismatch",
		});
		expect(stub.socket.open).toBe(false);
		expect(fixture.harness.manager.cellSinks.has(`local:${stub.socket.socketId}`)).toBe(false);
	});

	test("any frame before the hello closes the socket", async () => {
		const fixture = await makeFixture();
		const stub = fixture.open();

		sendClient(fixture, stub, view(String(SESSION_ID)));

		expect(frameCases(stub)).toEqual(["closed"]);
		expect(stub.frames[0]!.frame.value).toMatchObject({ reason: "hello required" });
	});

	test("a valid hello is ready with the granted sessions and its own generation", async () => {
		const fixture = await makeFixture();
		const first = fixture.open();
		const second = fixture.open();

		sendClient(fixture, first, hello(fixture, fixture.secret));
		sendClient(fixture, second, hello(fixture, fixture.secret));

		expect(frameCases(first)).toEqual(["ready"]);
		expect(readyFrame(first)).toMatchObject({
			workerFingerprint: WORKER_FP,
			sessionIds: [String(SESSION_ID), GRANTED_DEAD_SESSION],
		});
		expect(readyFrame(first).socketGeneration).toBeGreaterThan(0n);
		expect(readyFrame(second).socketGeneration).not.toBe(readyFrame(first).socketGeneration);
		expect(fixture.harness.manager.cellSinks.has(`local:${first.socket.socketId}`)).toBe(true);

		// A second hello on a live socket would silently re-point membership.
		sendClient(fixture, first, hello(fixture, fixture.secret));
		expect(frameCases(first)).toEqual(["ready", "closed"]);
		expect(first.frames[1]!.frame.value).toMatchObject({ reason: "duplicate hello" });
	});

	test("a view for a session outside the grant is refused", async () => {
		const fixture = await makeFixture();
		const stub = fixture.open();
		sendClient(fixture, stub, hello(fixture, fixture.secret));

		sendClient(fixture, stub, view(UNGRANTED_SESSION));

		expect(frameCases(stub)).toEqual(["ready", "terminalViewState"]);
		expect(stub.frames[1]!.frame.value).toMatchObject({
			sessionId: UNGRANTED_SESSION,
			status: TerminalViewStatus.REJECTED,
			reason: "terminal session is unavailable",
		});
		// A refused view creates no membership, so no stream was minted.
		expect(fixture.harness.manager.terminalStreams.get(CHANNEL_ID)).toBeUndefined();
		expect(stub.socket.open).toBe(true);
	});

	test("input takes the real write path and reports its outcome per session", async () => {
		const fixture = await makeFixture();
		const stub = fixture.open();
		sendClient(fixture, stub, hello(fixture, fixture.secret));

		sendClient(fixture, stub, input(String(SESSION_ID), 1n, "hello-pty"));
		await frameArrives(stub, "inputAccepted");
		expect(stub.frames.at(-1)!.frame.value).toMatchObject({
			sessionId: String(SESSION_ID),
			inputSeq: 1n,
			writtenBytes: "hello-pty".length,
		});

		sendClient(fixture, stub, input(GRANTED_DEAD_SESSION, 2n, "nowhere"));
		await frameArrives(stub, "inputRejected");
		expect(stub.frames.at(-1)!.frame.value).toMatchObject({
			sessionId: GRANTED_DEAD_SESSION,
			inputSeq: 2n,
			reason: "session is not live",
		});

		sendClient(fixture, stub, input(UNGRANTED_SESSION, 3n, "forbidden"));
		expect(stub.frames.at(-1)!.frame.value).toMatchObject({
			sessionId: UNGRANTED_SESSION,
			inputSeq: 3n,
			reason: "terminal session is unavailable",
		});
	});

	test("revoking the device closes its socket", async () => {
		const fixture = await makeFixture();
		const stub = fixture.open();
		sendClient(fixture, stub, hello(fixture, fixture.secret));

		fixture.sockets.revokeDevice(DEVICE);

		expect(frameCases(stub)).toEqual(["ready", "closed"]);
		expect(stub.frames[1]!.frame.value).toMatchObject({
			reason: "local terminal grant revoked",
		});
		expect(stub.socket.open).toBe(false);
		expect(fixture.grants.count()).toBe(0);
		expect(fixture.harness.manager.cellSinks.has(`local:${stub.socket.socketId}`)).toBe(false);
	});

	test("a socket that stops draining is dropped alone", async () => {
		const fixture = await makeFixture();
		const stalled = fixture.open();
		const healthy = fixture.open();
		sendClient(fixture, stalled, hello(fixture, fixture.secret));
		sendClient(fixture, healthy, hello(fixture, fixture.secret));
		sendClient(fixture, stalled, view(String(SESSION_ID)));
		sendClient(fixture, healthy, view(String(SESSION_ID)));
		// Bun answers -1 while a frame is buffered rather than written.
		stalled.sendResult = -1;
		const stalledSink = `local:${stalled.socket.socketId}`;
		const healthySink = `local:${healthy.socket.socketId}`;
		expect(fixture.harness.manager.cellSinks.has(stalledSink)).toBe(true);

		const delta = oversizedDelta();
		for (let attempt = 0; attempt < 8 && stalled.socket.open; attempt += 1) {
			sendCellDeltaToSinks(fixture.harness.manager, CHANNEL_ID, delta);
		}

		expect(stalled.closeReasons).toEqual(["local delivery overflow"]);
		expect(fixture.harness.manager.cellSinks.has(stalledSink)).toBe(false);
		expect(healthy.socket.open).toBe(true);
		expect(fixture.harness.manager.cellSinks.has(healthySink)).toBe(true);
		expect(frameCases(healthy).filter((name) => name === "cellGrid").length).toBeGreaterThan(0);
	});
});
