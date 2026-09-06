// Proves SessionManager channel creation drains before keeper replacement.
// Deferred keeper spawns exercise both fresh-spawn and reconnect-respawn paths
// without sleeps, while the coordinator handler supplies the update boundary.

import { create } from "@bufbuild/protobuf";
import { afterEach, describe, expect, test } from "bun:test";
import {
	KEEPER_EMPTY_BINDING_DIGEST,
	type JournaledKeeperUpdateV1,
} from "@roost/shared/keeper-update";
import { DKeeperUpdatePrepareSchema } from "@roost/shared/proto/worker_transport_pb";
import type { TerminalCore } from "@wterm/core";
import { asSessionId, asWorkerFp } from "@roost/shared/wire";
import { createKeeperUpdatePrepareHandler } from "../src/coord-link-keeper-update.ts";
import { getMultiplexedPool } from "../src/keeper/multiplexed-client.ts";
import { SessionManager } from "../src/session-manager.ts";
import { LifecycleTestSink } from "./lifecycle-test-sink.ts";

const SOURCE_DIGEST = "1".repeat(64);
const TARGET_DIGEST = "2".repeat(64);
const WORKER_FP = asWorkerFp("42".repeat(32));
const PRESERVE_SESSION_ID = asSessionId(
	"00000000-0000-4000-8000-000000000003",
);
const REPLACE_UPDATE = {
	admission: {
		classification: "keeper-restart-required",
		source_contract_digest: SOURCE_DIGEST,
		target_contract_digest: TARGET_DIGEST,
		expected_keeper_pid: 1234,
		expected_keeper_epoch: "00000000-0000-4000-8000-000000000001",
		expected_binding_digest: KEEPER_EMPTY_BINDING_DIGEST,
		required_action: "replace-empty",
	},
	source_contract: {
		protocol_version: 1,
		supported_features: [],
		required_features: [],
		implementation_digest: SOURCE_DIGEST,
		bun_abi: "test",
		platform: "linux",
		arch: "x64",
		build_sha: "source",
	},
	target_contract: {
		protocol_version: 1,
		supported_features: [],
		required_features: [],
		implementation_digest: TARGET_DIGEST,
		bun_abi: "test",
		platform: "linux",
		arch: "x64",
		build_sha: "target",
	},
} as const satisfies JournaledKeeperUpdateV1;
const PRESERVE_UPDATE = {
	admission: {
		classification: "worker-only-safe",
		source_contract_digest: SOURCE_DIGEST,
		target_contract_digest: SOURCE_DIGEST,
		expected_keeper_pid: 1234,
		expected_keeper_epoch: "00000000-0000-4000-8000-000000000001",
		expected_binding_digest: "3".repeat(64),
		required_action: "preserve",
	},
	source_contract: REPLACE_UPDATE.source_contract,
	target_contract: {
		...REPLACE_UPDATE.source_contract,
		build_sha: "target",
	},
} as const satisfies JournaledKeeperUpdateV1;


const pool = getMultiplexedPool();
const originalSpawn = pool.spawn;
const managers: SessionManager[] = [];

function freshManager(): SessionManager {
	const manager = new SessionManager({
		workerFp: WORKER_FP,
		sink: new LifecycleTestSink(),
		createTerminalCore: async (cols, rows) => ({
			getCols: () => cols,
			getRows: () => rows,
		}) as unknown as TerminalCore,
	});
	manager._startGitBranch = () => undefined;
	manager._startPorts = () => undefined;
	managers.push(manager);
	return manager;
}

function deferKeeperSpawn() {
	const entered = Promise.withResolvers<void>();
	const result = Promise.withResolvers<number>();
	pool.spawn = async function () {
		entered.resolve();
		return result.promise;
	};
	return { entered, result };
}

function updateRequest(
	update: JournaledKeeperUpdateV1 = REPLACE_UPDATE,
	coordinatorOpenSessionIds: readonly string[] = [],
) {
	return create(DKeeperUpdatePrepareSchema, {
		requestId: "keeper-channel-fence",
		journaledUpdateJson: JSON.stringify(update),
		direction: "target",
		maintenance: false,
		coordinatorOpenSessionIds: [...coordinatorOpenSessionIds],
	});
}

async function rejectionReason(
	promise: Promise<unknown>,
): Promise<unknown | null> {
	try {
		await promise;
		return null;
	} catch (error) {
		return error;
	}
}

afterEach(() => {
	pool.spawn = originalSpawn;
	for (const manager of managers.splice(0)) {
		for (const record of manager.allSessions()) manager.kill(record.channelId);
		manager.dispose();
	}
});

describe("SessionManager keeper channel creation gate", () => {
	test("preparation drains an admitted spawn, rejects a new spawn immediately, and stays closed on success", async () => {
		const manager = freshManager();
		const deferredSpawn = deferKeeperSpawn();
		const admittedSpawn = manager.spawnShell(
			process.cwd(),
			80,
			24,
			PRESERVE_SESSION_ID,
		);
		let updateActionCalls = 0;
		let maintenanceShutdownCalls = 0;
		let reconcileBoundaryCalls = 0;
		let reconcileRollbacks = 0;
		const prepareKeeperUpdate = createKeeperUpdatePrepareHandler({
			sessionManager: () => manager,
			acquireKeeperUpdateBoundary: () => async () => {
				reconcileBoundaryCalls += 1;
				return () => {
					reconcileRollbacks += 1;
				};
			},
			applyKeeperUpdateAction: async (action) => {
				updateActionCalls += 1;
				expect(action).toEqual({
					schema_version: 1,
					update: PRESERVE_UPDATE,
					direction: "target",
					coordinator_open_session_ids: [PRESERVE_SESSION_ID],
					worker_open_channel_ids: [1],
				});
				return {
					outcome: "preserved",
					keeper_pid: PRESERVE_UPDATE.admission.expected_keeper_pid,
					keeper_epoch: PRESERVE_UPDATE.admission.expected_keeper_epoch,
					binding_digest: PRESERVE_UPDATE.admission.expected_binding_digest,
				};
			},
			shutdownKeeperForMaintenance: async () => {
				maintenanceShutdownCalls += 1;
				return "shutdown";
			},
		});
		const preparation = prepareKeeperUpdate(
			updateRequest(PRESERVE_UPDATE, [PRESERVE_SESSION_ID]),
		);

		try {
			let rejectedReason: unknown;
			void manager.spawnShell(process.cwd()).catch((error) => {
				rejectedReason = error;
			});
			await Promise.resolve();
			expect(String(rejectedReason)).toContain(
				"keeper update preparation blocks channel creation",
			);

			await deferredSpawn.entered.promise;
			expect(updateActionCalls).toBe(0);
			expect(reconcileBoundaryCalls).toBe(0);
			deferredSpawn.result.resolve(4321);
			const admittedRecord = await admittedSpawn;
			expect(admittedRecord.childPid).toBe(4321);
			expect(await preparation).toEqual({
				outcome: "preserved",
				keeper_pid: PRESERVE_UPDATE.admission.expected_keeper_pid,
				keeper_epoch: PRESERVE_UPDATE.admission.expected_keeper_epoch,
				binding_digest: PRESERVE_UPDATE.admission.expected_binding_digest,
			});
			expect(updateActionCalls).toBe(1);
			expect(maintenanceShutdownCalls).toBe(0);
			expect(reconcileBoundaryCalls).toBe(1);
			expect(reconcileRollbacks).toBe(0);
			const closedReason = await rejectionReason(
				manager.spawnShell(process.cwd()),
			);
			expect(String(closedReason)).toContain(
				"keeper update preparation blocks channel creation",
			);
		} finally {
			deferredSpawn.result.reject(new Error("test cleanup"));
			await Promise.allSettled([admittedSpawn, preparation]);
		}
	});

	test("a failed admitted respawn drains before update failure and rollback reopens admission", async () => {
		const manager = freshManager();
		const deferredSpawn = deferKeeperSpawn();
		const sessionId = asSessionId("00000000-0000-4000-8000-000000000002");
		const admittedRespawn = manager.respawnIfMissing(
			sessionId,
			process.cwd(),
			80,
			24,
		);
		let updateActionCalls = 0;
		let reconcileBoundaryCalls = 0;
		let reconcileRollbacks = 0;
		const prepareKeeperUpdate = createKeeperUpdatePrepareHandler({
			sessionManager: () => manager,
			acquireKeeperUpdateBoundary: () => async () => {
				reconcileBoundaryCalls += 1;
				return () => {
					reconcileRollbacks += 1;
				};
			},
			applyKeeperUpdateAction: async () => {
				updateActionCalls += 1;
				throw new Error("injected keeper update failure");
			},
		});
		const preparation = prepareKeeperUpdate(updateRequest());

		try {
			await deferredSpawn.entered.promise;
			expect(updateActionCalls).toBe(0);
			expect(reconcileBoundaryCalls).toBe(0);
			deferredSpawn.result.reject(new Error("injected keeper spawn failure"));
			expect(String(await rejectionReason(admittedRespawn))).toContain(
				"injected keeper spawn failure",
			);
			expect(String(await rejectionReason(Promise.resolve(preparation)))).toContain(
				"injected keeper update failure",
			);
			expect(updateActionCalls).toBe(1);
			expect(reconcileBoundaryCalls).toBe(1);
			expect(reconcileRollbacks).toBe(1);

			pool.spawn = async function () {
				return 8765;
			};
			const reopenedRespawn = await manager.respawnIfMissing(
				sessionId,
				process.cwd(),
				80,
				24,
			);
			expect(reopenedRespawn.childPid).toBe(8765);
		} finally {
			deferredSpawn.result.reject(new Error("test cleanup"));
			await Promise.allSettled([admittedRespawn, preparation]);
		}
	});

	test("rejects a coordinator session set that differs from live worker state", async () => {
		const manager = freshManager();
		pool.spawn = async function () {
			return 9012;
		};
		await manager.spawnShell(
			process.cwd(),
			80,
			24,
			PRESERVE_SESSION_ID,
		);
		let updateActionCalls = 0;
		let reconcileRollbacks = 0;
		const prepareKeeperUpdate = createKeeperUpdatePrepareHandler({
			sessionManager: () => manager,
			acquireKeeperUpdateBoundary: () => async () => () => {
				reconcileRollbacks += 1;
			},
			applyKeeperUpdateAction: async () => {
				updateActionCalls += 1;
				return {
					outcome: "preserved",
					keeper_pid: PRESERVE_UPDATE.admission.expected_keeper_pid,
					keeper_epoch: PRESERVE_UPDATE.admission.expected_keeper_epoch,
					binding_digest: PRESERVE_UPDATE.admission.expected_binding_digest,
				};
			},
		});

		await expect(prepareKeeperUpdate(
			updateRequest(PRESERVE_UPDATE, []),
		)).rejects.toThrow("coordinator and worker open sessions changed");
		expect(updateActionCalls).toBe(0);
		expect(reconcileRollbacks).toBe(1);
		const reopened = await manager.spawnShell(
			process.cwd(),
			80,
			24,
			asSessionId("00000000-0000-4000-8000-000000000004"),
		);
		expect(reopened.childPid).toBe(9012);
	});
});
