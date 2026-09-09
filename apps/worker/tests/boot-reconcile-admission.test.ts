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
import { SessionEventStoreFatalError } from "../src/event-sink.ts";
import { getMultiplexedPool } from "../src/keeper/multiplexed-client.ts";
import { SessionEventTestSink } from "./session-event-test-sink.ts";
import { AgentReferenceAdmissionGate } from "../src/agent-status/reference-admission.ts";
import { TerminalCoreCapacityError } from "../src/terminal-core-capacity.ts";

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

function freshManager(sink: SessionEventTestSink): SessionManager {
	const manager = new SessionManager({ workerFp: WORKER_FP, sink });
	managers.push(manager);
	return manager;
}

function referenceReconcileDependencies() {
	return {
		referenceAdmission: new AgentReferenceAdmissionGate(),
		restoreAgentConversation: async () => ({
			status: "skipped" as const,
			reason: "disabled" as const,
		}),
		beforeRecoveryRead: async () => {},
	};
}

function sessionsResponse<
	const Sessions extends readonly { id: string }[],
>(sessions: Sessions) {
	return {
		sessions,
		recoveryMetadata: sessions.map((session) => ({
			sessionId: session.id,
			agentReference: undefined,
			agentReferenceClientSeq: 0n,
		})),
	};
}

function stubSessionAdmission(manager: SessionManager) {
	const advance = vi.fn(async () => {});
	const resume = vi.fn(async (
		_options: Parameters<SessionManager["resume"]>[0],
		reservation: Parameters<SessionManager["resume"]>[1],
	) => {
		if (!reservation) throw new Error("test reconcile omitted close reservation");
		manager.releaseSessionEvent(reservation);
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
		const restartKeeper = vi.spyOn(pool, "restartKeeper").mockImplementation(() => {});
		const sessionsGate = Promise.withResolvers<unknown>();
		const replayGate = Promise.withResolvers<void>();
		const sessionsList = vi.fn(() => sessionsGate.promise);
		const manager = freshManager(new SessionEventTestSink());
		const prepareKeeper = vi.fn(async () => {});
		const { reconcileOpenSessions } = setupReconcile({
			...referenceReconcileDependencies(),
			beforeRecoveryRead: () => replayGate.promise,
			client: () => clientWithSessionsList(sessionsList),
			workerFp: WORKER_FP,
			sessionMgr: manager,
			prepareKeeper,
		});

		const first = reconcileOpenSessions("boot");
		const overlapping = reconcileOpenSessions("keeper_death");
		expect(overlapping).toBe(first);
		manager.onKeeperDegraded?.();
		expect(restartKeeper).not.toHaveBeenCalled();
		const activation = bootActivation(() => first);
		const boot = activation.complete();

		expect(sessionsList).not.toHaveBeenCalled();
		replayGate.resolve();
		await Promise.resolve();
		await Promise.resolve();
		expect(sessionsList).toHaveBeenCalledTimes(1);
		expect(prepareKeeper).not.toHaveBeenCalled();
		expect(keeper.ensure).not.toHaveBeenCalled();
		expect(keeper.list).not.toHaveBeenCalled();
		expect(keeper.listFresh).not.toHaveBeenCalled();
		expect(keeper.kill).not.toHaveBeenCalled();
		expect(manager.strayReaperTimer).toBeNull();
		expect(activation.activateSnapshotProvider).not.toHaveBeenCalled();
		expect(activation.markReady).not.toHaveBeenCalled();

		sessionsGate.resolve(sessionsResponse([]));
		await expect(boot).resolves.toMatchObject({
			admitted: true,
			candidates: 0,
		});
		expect(prepareKeeper).toHaveBeenCalledTimes(1);
		expect(prepareKeeper).toHaveBeenCalledWith(new Set());
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

	test("defers a latched degraded signal until reconcile admission succeeds", async () => {
		spyOnKeeperMutation();
		const restartKeeper = vi.spyOn(pool, "restartKeeper").mockImplementation(() => {});
		const sessionsGate = Promise.withResolvers<unknown>();
		const laterGate = Promise.withResolvers<unknown>();
		let attempts = 0;
		const manager = freshManager(new SessionEventTestSink());
		const { reconcileOpenSessions } = setupReconcile({
			...referenceReconcileDependencies(),
			client: () => clientWithSessionsList(() => {
				attempts++;
				if (attempts === 1) return sessionsGate.promise;
				if (attempts === 3) return laterGate.promise;
				return Promise.resolve(sessionsResponse([]));
			}),
			workerFp: WORKER_FP,
			sessionMgr: manager,
			prepareKeeper: async () => {},
		});

		const reconciliation = reconcileOpenSessions("boot");
		manager.onKeeperDegraded?.();
		expect(restartKeeper).not.toHaveBeenCalled();
		sessionsGate.reject(new Error("coordinator admission unavailable"));
		await expect(reconciliation).resolves.toMatchObject({ admitted: false });
		await Promise.resolve();
		expect(restartKeeper).not.toHaveBeenCalled();
		manager.onKeeperDegraded?.();
		await Bun.sleep(0);
		expect(attempts).toBe(2);
		expect(restartKeeper).not.toHaveBeenCalled();
		const laterReconciliation = reconcileOpenSessions("keeper_death");
		manager.onKeeperDegraded?.();
		laterGate.reject(new Error("coordinator admission unavailable"));
		await expect(laterReconciliation).resolves.toMatchObject({ admitted: false });
		manager.onKeeperDegraded?.();
		await Bun.sleep(0);
		expect(attempts).toBe(4);
		expect(restartKeeper).not.toHaveBeenCalled();
	});

	test("a failed SessionsList cannot activate keeper, provider, or readiness and a later complete set succeeds", async () => {
		const keeper = spyOnKeeperMutation();
		const sink = new SessionEventTestSink();
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
			return sessionsResponse(OPEN_SESSIONS);
		});
		const admittedReservationCounts: number[] = [];
		const prepareKeeper = vi.fn(async () => {
			admittedReservationCounts.push(sink.active.size);
		});
		const { reconcileOpenSessions } = setupReconcile({
			...referenceReconcileDependencies(),
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
		expect(prepareKeeper).toHaveBeenCalledWith(
			new Set(OPEN_SESSIONS.map((session) => String(session.id))),
		);
		expect(operations.resume.mock.calls.map(([options]) => options.sessionId))
			.toEqual(OPEN_SESSIONS.map((session) => session.id));
		expect(sink.active.size).toBe(0);
		expect(activation.activateSnapshotProvider).toHaveBeenCalledTimes(1);
		expect(activation.markReady).toHaveBeenCalledTimes(1);
	});

	test("reservation exhaustion touches no keeper or session state and releases the whole batch for retry", async () => {
		const keeper = spyOnKeeperMutation();
		const sink = new SessionEventTestSink(5);
		const manager = freshManager(sink);
		const operations = stubSessionAdmission(manager);
		const sessionsList = vi.fn(async () => sessionsResponse(OPEN_SESSIONS));
		const admittedReservationCounts: number[] = [];
		const prepareKeeper = vi.fn(async () => {
			admittedReservationCounts.push(sink.active.size);
		});
		const { reconcileOpenSessions } = setupReconcile({
			...referenceReconcileDependencies(),
			client: () => clientWithSessionsList(sessionsList),
			workerFp: WORKER_FP,
			sessionMgr: manager,
			prepareKeeper,
		});
		const activation = bootActivation(() => reconcileOpenSessions("boot"));

		await expect(activation.complete()).rejects.toThrow(
			"session event outbox full",
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
		expect(prepareKeeper).toHaveBeenCalledWith(
			new Set(OPEN_SESSIONS.map((session) => String(session.id))),
		);
		expect(operations.resume).toHaveBeenCalledTimes(2);
		expect(sink.active.size).toBe(0);
		expect(activation.activateSnapshotProvider).toHaveBeenCalledTimes(1);
		expect(activation.markReady).toHaveBeenCalledTimes(1);
	});

	test("capacity refusal leaves a missing coordinator session unresolved", async () => {
		spyOnKeeperMutation();
		const sink = new SessionEventTestSink();
		const manager = freshManager(sink);
		const advance = vi.fn(async () => {});
		const resume = vi.fn(async (
			_options: Parameters<SessionManager["resume"]>[0],
			reservation: Parameters<SessionManager["resume"]>[1],
		) => {
			if (!reservation) throw new Error("test reconcile omitted close reservation");
			manager.releaseSessionEvent(reservation);
			return false;
		});
		const respawn = vi.fn(async () => {
			throw new TerminalCoreCapacityError("replacement");
		});
		const tombstone = vi.spyOn(manager, "emitClosedTombstone");
		manager.advanceChannelCounterPastKeeper = advance;
		manager.resume = resume;
		manager.respawn = respawn;
		manager.reapStrayKeeperChannels = vi.fn(async () => 0);
		const { reconcileOpenSessions } = setupReconcile({
			...referenceReconcileDependencies(),
			client: () => clientWithSessionsList(() => sessionsResponse([
				OPEN_SESSIONS[0],
			])),
			workerFp: WORKER_FP,
			sessionMgr: manager,
			prepareKeeper: async () => {},
		});
		const activation = bootActivation(() => reconcileOpenSessions("boot"));

		await expect(activation.complete()).rejects.toThrow(
			"terminal core capacity exhausted",
		);
		expect(resume).toHaveBeenCalledTimes(1);
		expect(respawn).toHaveBeenCalledTimes(1);
		expect(tombstone).not.toHaveBeenCalled();
		expect(sink.active.size).toBe(0);
	});

	test("a rejected in-flight reconcile atomically reopens its keeper update boundary", async () => {
		const keeper = spyOnKeeperMutation();
		const sink = new SessionEventTestSink();
		const reserveSessionEvent = sink.reserveSessionEvent.bind(sink);
		let rejectNextReservation = true;
		sink.reserveSessionEvent = (kind) => {
			if (rejectNextReservation) {
				rejectNextReservation = false;
				throw new SessionEventStoreFatalError(
					"injected fatal reconcile failure",
				);
			}
			return reserveSessionEvent(kind);
		};
		const manager = freshManager(sink);
		const operations = stubSessionAdmission(manager);
		const sessionsGate = Promise.withResolvers<unknown>();
		const sessionsList = vi.fn(() => sessionsGate.promise);
		const prepareKeeper = vi.fn(async () => {});
		const {
			reconcileOpenSessions,
			acquireKeeperUpdateBoundary,
		} = setupReconcile({
			...referenceReconcileDependencies(),
			client: () => clientWithSessionsList(sessionsList),
			workerFp: WORKER_FP,
			sessionMgr: manager,
			prepareKeeper,
		});

		const reconcile = reconcileOpenSessions("keeper_death");
		const boundary = acquireKeeperUpdateBoundary();
		const reconcileFailure = reconcile.catch((error) => error);
		const boundaryFailure = boundary.catch((error) => error);
		sessionsGate.resolve(sessionsResponse(OPEN_SESSIONS));
		expect(String(await reconcileFailure)).toContain(
			"injected fatal reconcile failure",
		);
		expect(String(await boundaryFailure)).toContain(
			"injected fatal reconcile failure",
		);
		expect(prepareKeeper).not.toHaveBeenCalled();
		expect(operations.resume).not.toHaveBeenCalled();

		await expect(reconcileOpenSessions("retry")).resolves.toMatchObject({
			admitted: true,
			candidates: 2,
			resumed: 2,
		});
		expect(sessionsList).toHaveBeenCalledTimes(2);
		expect(operations.resume).toHaveBeenCalledTimes(2);
		expect(keeper.ensure).toHaveBeenCalledTimes(1);
		expect(sink.active.size).toBe(0);
	});
});
