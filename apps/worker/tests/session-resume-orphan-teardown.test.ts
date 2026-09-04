// Failed-ADOPTION teardown in resume(): when adoption throws AFTER the
// surviving keeper channel was re-attached (invalid geometry, history-contract
// violation), the channel stays live and attached while NO SessionRecord
// exists. The old catch merely swapped the staging queue for the live
// callbacks and returned, so every PtyOut the orphan then emitted hit
// emit_no_session; KEEPER_DEGRADED_THRESHOLD (5) crossed → onKeeperDegraded →
// restartKeeper SIGTERMs every healthy session. Pins the agreed repair: the
// catch kills the adopted channel via the pool's own kill primitive,
// registers it recentlyClosed (post-close tail gate + stray-sweep ownership),
// emits the standard closed tombstone coord already understands, and still
// returns false so boot reconciliation respawns under the same sid.
//
// Only the pool's probe surface is stubbed — the catch teardown, the tail
// gate in emitUpstreamChunk, and the real inbound dispatch route
// (pool.channels → callbacks, as keeper-pool-lifecycle drives it) are the
// production code paths.

import { describe, test, expect, afterEach } from "bun:test";
import { SessionManager } from "../src/session-manager.ts";
import { asSessionId, asChannelId, asWorkerFp } from "@roost/shared/wire";
import type { SessionEvent } from "@roost/shared/wire";
import { MuxFrameType } from "../src/keeper/protocol.ts";
import { getMultiplexedPool } from "../src/keeper/multiplexed-client.ts";
import type {
	KeeperTerminalState,
	MultiplexedKeeperPool,
} from "../src/keeper/multiplexed-client.ts";
import { installAutoKeeper, type FakeKeeper } from "./keeper-fake-pool.ts";
import type { ShellSpec } from "../src/shell-spec.ts";
import { LifecycleTestSink } from "./lifecycle-test-sink.ts";

const SESSION_ID = asSessionId("9d2e6c31-4444-4555-9666-777788889999");
const CHANNEL_ID = 23;
const CHILD_PID = 55332;

const SHELL_SPEC: ShellSpec = {
	version: 1,
	platform: "linux",
	executable: "/bin/sh",
	argv: [],
	cwd: "/",
	env: {},
};

const VALID_TERMINAL_STATE: KeeperTerminalState = {
	headSeq: 0,
	cols: 80,
	rows: 24,
	highestResizeSeq: 0,
	appliedResizeSeq: 0,
};

interface Fixture {
	mgr: SessionManager;
	pool: MultiplexedKeeperPool;
	keeper: FakeKeeper;
	/** Everything the worker emitted to the coord sink, in order. */
	events: SessionEvent[];
	state: { degradedCalls: number };
	dispose(): void;
}

let live: Fixture | null = null;

afterEach(() => {
	live?.dispose();
	live = null;
});

/** Drive resume() against an adopting keeper whose terminal-state report is
 *  controlled by `terminalState`: null makes the geometry validator throw, so
 *  the catch runs with the channel ALREADY staged — exactly the orphan window
 *  this fix closes. */
async function resumeFixture(opts: {
	terminalState: KeeperTerminalState | null;
}): Promise<Fixture> {
	// Before the manager: its constructor dials the pool, and a real keeper
	// adopted by that dial would replace the fake socket mid-test.
	const keeper = installAutoKeeper({ cols: 80, rows: 24 });
	const pool = getMultiplexedPool();
	const priorListChannels = pool.listChannels;
	const priorReattach = pool.reattach;
	const priorGetHistoryRecords = pool.getHistoryRecords;
	const priorGetTerminalState = pool.getTerminalState;
	pool.listChannels = async () => [{ channelId: CHANNEL_ID, pid: CHILD_PID }];
	const sink = new LifecycleTestSink();
	const events = sink.events;
	const state = { degradedCalls: 0 };
	pool.reattach = (channelId, callbacks) => {
		// Mirror reattachChannel's registration so later PtyOut frames dispatch
		// through the REAL inbound route (pool.channels lookup), not a captured
		// closure.
		pool.channels.set(channelId, callbacks);
	};
	pool.getHistoryRecords = async () => ({
		headSeq: 0,
		baseCols: 80,
		baseRows: 24,
		records: [],
	});
	pool.getTerminalState = async () => opts.terminalState;

	const mgr = new SessionManager({
		workerFp: asWorkerFp("22".repeat(32)),
		sink,
		sendBinaryUpstream: () => "sent",
		sendCellGridUpstream: () => "sent",
	});
	// Git/port probes spawn subprocesses and say nothing about adoption.
	mgr._startGitBranch = () => {};
	mgr._startPorts = () => {};
	mgr.setOnKeeperDegraded(() => { state.degradedCalls++; });

	const resumed = await mgr.resume({
		sessionId: SESSION_ID,
		channelId: asChannelId(CHANNEL_ID),
		kind: "shell",
		cwd: "/",
		shellSpec: SHELL_SPEC,
	});
	expect(resumed).toBe(opts.terminalState !== null);

	const fixture: Fixture = {
		mgr,
		pool,
		keeper,
		events,
		state,
		dispose() {
			mgr.dispose();
			pool.listChannels = priorListChannels;
			pool.reattach = priorReattach;
			pool.getHistoryRecords = priorGetHistoryRecords;
			pool.getTerminalState = priorGetTerminalState;
			pool.channels.delete(CHANNEL_ID);
			keeper.restore();
		},
	};
	live = fixture;
	return fixture;
}

describe("resume() failed-adoption teardown", () => {
	test("a forced post-reattach rejection kills the adopted keeper channel", async () => {
		const f = await resumeFixture({ terminalState: null });

		// The pool's OWN kill primitive ran against the adopted channel — the
		// keeper tears down the PTY instead of leaving it emitting into a void.
		const kills = f.keeper.writes.filter(
			(w) => w.type === MuxFrameType.KillChild && w.channelId === CHANNEL_ID,
		);
		expect(kills.length).toBeGreaterThanOrEqual(1);
	});

	test("the killed channel is registered recentlyClosed for the tail gate", async () => {
		const f = await resumeFixture({ terminalState: null });

		// emitUpstreamChunk consults this BEFORE counting emit_no_session, so the
		// entry is what keeps racing frames benign until the stray sweep reaps.
		expect(typeof f.mgr.recentlyClosed.get(CHANNEL_ID)).toBe("number");
	});

	test("coord receives the standard closed tombstone for the unadoptable row", async () => {
		const f = await resumeFixture({ terminalState: null });

		// Same variant a channel dying mid-resume produces (_onTransition /
		// emitClosedTombstone): exit_code unknown because WE chose the kill.
		expect(f.events).toContainEqual({
			kind: "closed",
			session_id: SESSION_ID,
			exit_code: null,
			ts: expect.any(Number),
		});
	});

	test("orphan PtyOut frames afterwards are neither handled nor counted toward degradation", async () => {
		const f = await resumeFixture({ terminalState: null });
		expect(f.mgr.sessions.has(CHANNEL_ID)).toBe(false);

		// Real dispatch route for keeper PtyOut frames (keeper-pool-lifecycle):
		// whatever the catch left attached must drop bytes via the recentlyClosed
		// tail gate, NOT feed _noSessionBurst → keeper.degraded → restartKeeper
		// SIGTERM storm over every healthy session.
		const callbacks = f.pool.channels.get(CHANNEL_ID);
		expect(callbacks).toBeDefined();
		for (let i = 0; i < 10; i++) callbacks!.onOutput(Buffer.from(`orphan-${i}`));

		expect(f.mgr.sessions.has(CHANNEL_ID)).toBe(false); // still record-less
		expect(
			(f.mgr as unknown as { _noSessionBurst: number[] })._noSessionBurst,
		).toHaveLength(0);
		expect(f.state.degradedCalls).toBe(0);
	});

	test("a successful adoption still writes no KillChild and emits no tombstone", async () => {
		const f = await resumeFixture({ terminalState: VALID_TERMINAL_STATE });

		// Guard against overreach: only FAILED adoptions tear down + tombstone.
		expect(f.mgr.sessions.has(CHANNEL_ID)).toBe(true);
		expect(
			f.keeper.writes.filter((w) => w.type === MuxFrameType.KillChild),
		).toHaveLength(0);
		expect(f.events.filter((e) => e.kind === "closed")).toHaveLength(0);
		expect(f.mgr.recentlyClosed.has(CHANNEL_ID)).toBe(false);
	});
});
