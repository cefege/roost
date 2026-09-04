// Boot reconciliation is a fail-closed admission barrier for keeper and snapshot state.
// These tests hold coordinator admission open or fail it before proving that no keeper
// maintenance, local session mutation, snapshot provider, or readiness leaks past it.

import { afterEach, describe, expect, test, vi } from "bun:test";
import { tmpdir } from "node:os";
import { asSessionId, asWorkerFp } from "@roost/shared/wire";
import type { CoordClient } from "../src/coord-client.ts";
import {
	setupReconcile,
	type ReconcileAdmissionOutcome,
} from "../src/boot-reconcile.ts";
import { completeWorkerBootAdmission } from "../src/main.ts";
import { SessionManager } from "../src/session-manager.ts";
import { getMultiplexedPool } from "../src/keeper/multiplexed-client.ts";
import { LifecycleTestSink } from "./lifecycle-test-sink.ts";

const WORKER_FP = asWorkerFp("42".repeat(32));
const OPEN_SESSIONS = [
	{
		id: asSessionId("00000000-0000-4000-8000-000000000001"),
		channel: 11,
		kind: "shell",
		cwd: tmpdir(),
	},
	{
		id: asSessionId("00000000-0000-4000-8000-000000000002"),
		channel: 12,
		kind: "shell",
		cwd: tmpdir(),
	},
] as const;

const pool = getMultiplexedPool();
const managers: SessionManager[] = [];

afterEach(() => {
	for (const manager of managers) manager.dispose();
	managers.length = 0;
	pool._onKeeperDeath = null;
	vi.restoreAllMocks();
});

function clientWithSessionsList(
	sessionsList: (
		...args: Parameters<CoordClient["sessionsList"]>
	) => Promise<unknown>,
): CoordClient {
	return { sessionsList } as unknown as CoordClient;
}

function freshManager(sink: LifecycleTestSink): SessionManager {
	const manager = new SessionManager({ workerFp: WORKER_FP, sink });
	managers.push(manager);
	return manager;
}

function stubSessionAdmission(manager: SessionManager) {
	const advance = vi.fn(async () => {});
	const resume = vi.fn(async (
		_options: Parameters<SessionManager["resume"]>[0],
		reservation: Parameters<SessionManager["resume"]>[1],
	) => {
		if (!reservation) throw new Error("test reconcile omitted close reservation");
		manager.releaseLifecycleEvent(reservation);
		return true;
	});
	const respawn = vi.fn(async () => {});
	const reap = vi.fn(async () => 0);
	manager.advanceChannelCounterPastKeeper = advance;
	manager.resume = resume;
	manager.respawn = respawn;
	manager.reapStrayKeeperChannels = reap;
	return { advance, resume, respawn, reap };
}

function spyOnKeeperMutation() {
	const ensure = vi.spyOn(pool, "ensure").mockResolvedValue();
	const list = vi.spyOn(pool, "listChannels").mockResolvedValue([]);
	const listFresh = vi.spyOn(pool, "listChannelsFresh").mockResolvedValue([]);
	const kill = vi.spyOn(pool, "kill").mockImplementation(() => {});
	return { ensure, list, listFresh, kill };
}

function bootActivation(reconcile: () => Promise<ReconcileAdmissionOutcome>) {
	const activateSnapshotProvider = vi.fn();
	const markReady = vi.fn();
	return {
		activateSnapshotProvider,
		markReady,
		complete: () => completeWorkerBootAdmission({
			reconcile,
			activateSnapshotProvider,
			markReady,
		}),
	};
}

describe("worker boot reconciliation admission", () => {
	test("overlapping callers join the delayed boot reconcile and maintenance stays dormant", async () => {
		const keeper = spyOnKeeperMutation();
		const sessionsGate = Promise.withResolvers<{ sessions: [] }>();
		const sessionsList = vi.fn(() => sessionsGate.promise);
		const manager = freshManager(new LifecycleTestSink());
		const prepareKeeper = vi.fn(async () => {});
		const { reconcileOpenSessions } = setupReconcile({
			client: () => clientWithSessionsList(sessionsList),
			workerFp: WORKER_FP,
			sessionMgr: manager,
			prepareKeeper,
		});

		const first = reconcileOpenSessions("boot");
		const overlapping = reconcileOpenSessions("keeper_death");
		expect(overlapping).toBe(first);
		const activation = bootActivation(() => first);
		const boot = activation.complete();

		expect(sessionsList).toHaveBeenCalledTimes(1);
		expect(prepareKeeper).not.toHaveBeenCalled();
		expect(keeper.ensure).not.toHaveBeenCalled();
		expect(keeper.list).not.toHaveBeenCalled();
		expect(keeper.listFresh).not.toHaveBeenCalled();
		expect(keeper.kill).not.toHaveBeenCalled();
		expect(manager.strayReaperTimer).toBeNull();
		expect(activation.activateSnapshotProvider).not.toHaveBeenCalled();
		expect(activation.markReady).not.toHaveBeenCalled();

		sessionsGate.resolve({ sessions: [] });
		await expect(boot).resolves.toMatchObject({
			admitted: true,
			candidates: 0,
		});
		expect(prepareKeeper).toHaveBeenCalledTimes(1);
		expect(keeper.ensure).toHaveBeenCalledTimes(1);
		expect(keeper.list).toHaveBeenCalledTimes(1);
		expect(keeper.listFresh).toHaveBeenCalledTimes(1);
		expect(manager.strayReaperTimer).not.toBeNull();
		const initialReaperTimer = manager.strayReaperTimer;
		await manager.startPostAdmissionMaintenance();
		expect(manager.strayReaperTimer).toBe(initialReaperTimer);
		expect(keeper.ensure).toHaveBeenCalledTimes(1);
		expect(activation.activateSnapshotProvider).toHaveBeenCalledTimes(1);
		expect(activation.markReady).toHaveBeenCalledTimes(1);
	});

	test("a failed SessionsList cannot activate keeper, provider, or readiness and a later complete set succeeds", async () => {
		const keeper = spyOnKeeperMutation();
		const sink = new LifecycleTestSink();
		const manager = freshManager(sink);
		const operations = stubSessionAdmission(manager);
		let sessionsListAttempts = 0;
		const sessionsList = vi.fn(async (
			..._args: Parameters<CoordClient["sessionsList"]>
		) => {
			sessionsListAttempts++;
			if (sessionsListAttempts === 1) {
				throw new Error("coordinator admission unavailable");
			}
			return { sessions: OPEN_SESSIONS };
		});
		const admittedReservationCounts: number[] = [];
		const prepareKeeper = vi.fn(async () => {
			admittedReservationCounts.push(sink.active.size);
		});
		const { reconcileOpenSessions } = setupReconcile({
			client: () => clientWithSessionsList(sessionsList),
			workerFp: WORKER_FP,
			sessionMgr: manager,
			prepareKeeper,
		});
		const activation = bootActivation(() => reconcileOpenSessions("boot"));

		await expect(activation.complete()).rejects.toThrow(
			"coordinator admission unavailable",
		);
		expect(sessionsList.mock.calls[0]?.[1]).toEqual({ timeoutMs: 10_000 });
		expect(prepareKeeper).not.toHaveBeenCalled();
		expect(keeper.ensure).not.toHaveBeenCalled();
		expect(keeper.list).not.toHaveBeenCalled();
		expect(keeper.listFresh).not.toHaveBeenCalled();
		expect(keeper.kill).not.toHaveBeenCalled();
		expect(operations.advance).not.toHaveBeenCalled();
		expect(operations.resume).not.toHaveBeenCalled();
		expect(operations.respawn).not.toHaveBeenCalled();
		expect(operations.reap).not.toHaveBeenCalled();
		expect(manager.strayReaperTimer).toBeNull();
		expect(activation.activateSnapshotProvider).not.toHaveBeenCalled();
		expect(activation.markReady).not.toHaveBeenCalled();

		await expect(activation.complete()).resolves.toMatchObject({
			admitted: true,
			candidates: 2,
			resumed: 2,
		});
		expect(admittedReservationCounts).toEqual([6]);
		expect(operations.resume.mock.calls.map(([options]) => options.sessionId))
			.toEqual(OPEN_SESSIONS.map((session) => session.id));
		expect(sink.active.size).toBe(0);
		expect(activation.activateSnapshotProvider).toHaveBeenCalledTimes(1);
		expect(activation.markReady).toHaveBeenCalledTimes(1);
	});

	test("reservation exhaustion touches no keeper or session state and releases the whole batch for retry", async () => {
		const keeper = spyOnKeeperMutation();
		const sink = new LifecycleTestSink(5);
		const manager = freshManager(sink);
		const operations = stubSessionAdmission(manager);
		const sessionsList = vi.fn(async () => ({ sessions: OPEN_SESSIONS }));
		const admittedReservationCounts: number[] = [];
		const prepareKeeper = vi.fn(async () => {
			admittedReservationCounts.push(sink.active.size);
		});
		const { reconcileOpenSessions } = setupReconcile({
			client: () => clientWithSessionsList(sessionsList),
			workerFp: WORKER_FP,
			sessionMgr: manager,
			prepareKeeper,
		});
		const activation = bootActivation(() => reconcileOpenSessions("boot"));

		await expect(activation.complete()).rejects.toThrow(
			"session lifecycle outbox full",
		);
		expect(sink.active.size).toBe(0);
		expect(prepareKeeper).not.toHaveBeenCalled();
		expect(keeper.ensure).not.toHaveBeenCalled();
		expect(keeper.list).not.toHaveBeenCalled();
		expect(keeper.listFresh).not.toHaveBeenCalled();
		expect(keeper.kill).not.toHaveBeenCalled();
		expect(operations.advance).not.toHaveBeenCalled();
		expect(operations.resume).not.toHaveBeenCalled();
		expect(operations.respawn).not.toHaveBeenCalled();
		expect(operations.reap).not.toHaveBeenCalled();
		expect(manager.strayReaperTimer).toBeNull();
		expect(activation.activateSnapshotProvider).not.toHaveBeenCalled();
		expect(activation.markReady).not.toHaveBeenCalled();

		sink.capacity = 6;
		await expect(activation.complete()).resolves.toMatchObject({
			admitted: true,
			candidates: 2,
			resumed: 2,
		});
		expect(admittedReservationCounts).toEqual([6]);
		expect(operations.resume).toHaveBeenCalledTimes(2);
		expect(sink.active.size).toBe(0);
		expect(activation.activateSnapshotProvider).toHaveBeenCalledTimes(1);
		expect(activation.markReady).toHaveBeenCalledTimes(1);
	});
});
