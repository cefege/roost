// Coordinator keeper-update preparation is serialized across downstream requests.
// The handler closes channel creation, joins boot reconciliation, and verifies
// the coordinator's canonical session snapshot before applying either action.

import {
	JournaledKeeperUpdateV1Schema,
	KeeperCoordinatorOpenSessionIdsSchema,
} from "@roost/shared/keeper-update";
import {
	applyJournaledKeeperUpdateAction,
	shutdownEmptyKeeperForMaintenance,
} from "./keeper/update-admission.ts";
import type { SessionManager } from "./session-manager.ts";
import type { CoordLinkDeps } from "./transport/coord-link.ts";

type KeeperUpdatePrepareHandler = NonNullable<
	CoordLinkDeps["onKeeperUpdatePrepare"]
>;
type AcquireKeeperUpdateBoundary = () => Promise<() => void>;

export function createKeeperUpdatePrepareHandler(deps: {
	sessionManager: () => SessionManager;
	acquireKeeperUpdateBoundary: () => AcquireKeeperUpdateBoundary | null;
	applyKeeperUpdateAction?: typeof applyJournaledKeeperUpdateAction;
	shutdownKeeperForMaintenance?: typeof shutdownEmptyKeeperForMaintenance;
}): KeeperUpdatePrepareHandler {
	const applyKeeperUpdateAction =
		deps.applyKeeperUpdateAction ?? applyJournaledKeeperUpdateAction;
	const shutdownKeeperForMaintenance =
		deps.shutdownKeeperForMaintenance ?? shutdownEmptyKeeperForMaintenance;
	let preparationTail = Promise.resolve();
	const serializePreparation = <Result>(
		operation: () => Promise<Result>,
	): Promise<Result> => {
		const result = preparationTail.then(operation, operation);
		preparationTail = result.then(
			() => undefined,
			() => undefined,
		);
		return result;
	};

	return (request) => {
		const sessionManager = deps.sessionManager();
		// serializePreparation defers even its first operation to a microtask.
		// Close admission before joining that tail so a spawn issued in this
		// request's call stack cannot cross the preparation boundary.
		const admissionPreparation =
			sessionManager.beginKeeperUpdatePreparation();
		return serializePreparation(async () => {
			const rollbackAdmission = await admissionPreparation;
			let rollbackReconcile: (() => void) | null = null;
			try {
				const acquireBoundary = deps.acquireKeeperUpdateBoundary();
				if (!acquireBoundary) {
					throw new Error("keeper update reconcile boundary is unavailable");
				}
				rollbackReconcile = await acquireBoundary();
				const coordinatorOpenSessionIds =
					KeeperCoordinatorOpenSessionIdsSchema.parse(
						request.coordinatorOpenSessionIds,
					);
				const workerSessions = sessionManager.allSessions();
				const workerOpenSessionIds =
					KeeperCoordinatorOpenSessionIdsSchema.parse(
						workerSessions.map(record => String(record.sessionId)).sort(),
					);
				const workerOpenChannelIds = workerSessions
					.map(record => Number(record.channelId))
					.sort((left, right) => left - right);
				if (
					coordinatorOpenSessionIds.length !== workerOpenSessionIds.length
					|| coordinatorOpenSessionIds.some(
						(sessionId, index) => sessionId !== workerOpenSessionIds[index],
					)
				) {
					throw new Error(
						"coordinator and worker open sessions changed after update admission",
					);
				}
				if (request.maintenance) {
					if (
						request.journaledUpdateJson !== undefined
						|| request.direction !== ""
						|| coordinatorOpenSessionIds.length !== 0
					) {
						throw new Error("keeper maintenance request is malformed");
					}
					const outcome = await shutdownKeeperForMaintenance();
					rollbackReconcile();
					rollbackReconcile = null;
					rollbackAdmission();
					return { outcome };
				}
				if (
					request.journaledUpdateJson === undefined
					|| (request.direction !== "source" && request.direction !== "target")
				) {
					throw new Error("journaled keeper update request is malformed");
				}
				const update = JournaledKeeperUpdateV1Schema.parse(
					JSON.parse(request.journaledUpdateJson),
				);
				return await applyKeeperUpdateAction({
					schema_version: 1,
					update,
					direction: request.direction,
					coordinator_open_session_ids: coordinatorOpenSessionIds,
					worker_open_channel_ids: workerOpenChannelIds,
				});
			} catch (error) {
				rollbackAdmission();
				rollbackReconcile?.();
				throw error;
			}
		});
	};
}
