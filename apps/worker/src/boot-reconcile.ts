// Worker boot admission reconciles the coordinator's complete open-session set
// before any keeper or SessionManager mutation. It also serializes later
// keeper-death reconciliation and owns degraded-keeper remediation wiring.

import { signal } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import type { WorkerFp } from "@roost/shared/wire";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import type { CoordClient } from "./coord-client.ts";
import type { SessionManager } from "./session-manager.ts";
import type { AgentReferenceAdmissionGate } from "./agent-status/reference-admission.ts";
import {
	reconcileCoordinatorSessions,
	type ReconcileAdmissionOutcome,
} from "./boot-session-reconcile.ts";
export type {
	ReconcileAdmissionFailure,
	ReconcileAdmissionOutcome,
	ReconcileAdmissionSuccess,
} from "./boot-session-reconcile.ts";


export function setupReconcile(deps: {
	client: () => CoordClient;
	workerFp: WorkerFp;
	sessionMgr: SessionManager;
	prepareKeeper: (
		coordinatorOpenSessionIds: ReadonlySet<string>,
	) => Promise<void>;
	referenceAdmission: Pick<AgentReferenceAdmissionGate, "runExclusive">;
	beforeRecoveryRead: () => Promise<void>;
	onReconciled?: (reconciledAtMs: number) => void;
	onReconcileStarted?: () => void;
}): {
	reconcileOpenSessions: (
		reason: string,
	) => Promise<ReconcileAdmissionOutcome>;
	acquireKeeperUpdateBoundary: () => Promise<() => void>;
} {
	const {
		client,
		workerFp,
		sessionMgr,
		prepareKeeper,
		referenceAdmission,
		beforeRecoveryRead,
		onReconcileStarted,
		onReconciled,
	} = deps;
	let reconcileInFlight: Promise<ReconcileAdmissionOutcome> | null = null;
	let keeperUpdateBlocked = false;
	let pendingDegradedRemediation = false;
	let lastReconcileMs = 0;
	let reconcileAdmitted = false;
	const KEEPER_DEGRADED_REMEDIATION_GRACE_MS = 90_000;
	const KEEPER_RESTART_BUDGET = 2;
	const KEEPER_RESTART_BUDGET_WINDOW_MS = 5 * 60_000;
	const keeperRestarts: number[] = [];

	const runReconcile = async (
		reason: string,
	): Promise<ReconcileAdmissionOutcome> => {
		reconcileAdmitted = false;
		onReconcileStarted?.();
		return reconcileCoordinatorSessions({
			client,
			workerFp,
			sessionMgr,
			prepareKeeper,
			referenceRecoveryAdmission: (read) =>
				referenceAdmission.runExclusive(async () => {
					await beforeRecoveryRead();
					return read();
				}),
			onReconciled: (reconciledAtMs) => {
				reconcileAdmitted = true;
				lastReconcileMs = reconciledAtMs;
				onReconciled?.(reconciledAtMs);
			},
		}, reason);
	};

	const reconcileOpenSessions = (
		reason: string,
	): Promise<ReconcileAdmissionOutcome> => {
		if (keeperUpdateBlocked) {
			return Promise.resolve({
				admitted: false,
				error: new Error("keeper update preparation blocks reconciliation"),
			});
		}
		if (reconcileInFlight) {
			log.info("worker", "reconcile_joined_inflight", { reason });
			return reconcileInFlight;
		}
		const current = runReconcile(reason);
		reconcileInFlight = current;
		void current.then(
			(outcome) => {
				if (reconcileInFlight !== current) return;
				reconcileInFlight = null;
				if (pendingDegradedRemediation && outcome.admitted) {
					pendingDegradedRemediation = false;
					remediateDegradedKeeper();
				}
			},
			() => {
				if (reconcileInFlight === current) reconcileInFlight = null;
			},
		);
		return current;
	};

	const acquireKeeperUpdateBoundary = async (): Promise<() => void> => {
		const changed = !keeperUpdateBlocked;
		keeperUpdateBlocked = true;
		const active = reconcileInFlight;
		try {
			if (active) await active;
		} catch (error) {
			if (changed) keeperUpdateBlocked = false;
			throw error;
		}
		let released = false;
		return () => {
			if (released || !changed) return;
			released = true;
			keeperUpdateBlocked = false;
		};
	};

	// Mid-life keeper death drives the same serialized admission path. A
	// recoverable coordinator failure remains visible in runReconcile's log and
	// the next trigger can retry after the shared promise settles.
	getMultiplexedPool().setOnKeeperDeath(() => {
		if (sessionMgr.keeperUpdatePrepared) {
			log.info("worker", "keeper_death_reconcile_suppressed", {});
			return;
		}
		log.warn("worker", "keeper_death_reconcile", {});
		void reconcileOpenSessions("keeper_death").catch((error) => {
			// Fatal durability failures must reach the worker's uncaught handler;
			// recoverable admission failures resolve with admitted:false.
			queueMicrotask(() => {
				throw error;
			});
		});
	});

	// Self-heal a DEGRADED survivor keeper outside the post-reconcile grace
	// window. The bounded restart budget prevents repeated PTY destruction.
	function remediateDegradedKeeper(): void {
		if (sessionMgr.keeperUpdatePrepared) {
			log.info("worker", "keeper_degraded_restart_suppressed", {});
			return;
		}
		const sinceReconcile = Date.now() - lastReconcileMs;
		if (sinceReconcile < KEEPER_DEGRADED_REMEDIATION_GRACE_MS) {
			log.info("worker", "keeper_degraded_skip_transient", {
				since_reconcile_ms: sinceReconcile,
			});
			return;
		}
		const now = Date.now();
		const windowStart = now - KEEPER_RESTART_BUDGET_WINDOW_MS;
		while (keeperRestarts.length && keeperRestarts[0]! < windowStart)
			keeperRestarts.shift();
		if (keeperRestarts.length >= KEEPER_RESTART_BUDGET) {
			signal("keeper.degraded_unrecoverable", {
				restarts: keeperRestarts.length,
				window_ms: KEEPER_RESTART_BUDGET_WINDOW_MS,
				cooldownKey: "keeper",
			});
			log.error("worker", "keeper_degraded_unrecoverable", {
				restarts: keeperRestarts.length,
			});
			return;
		}
		keeperRestarts.push(now);
		log.warn("worker", "keeper_degraded_restart", {
			since_reconcile_ms: sinceReconcile,
			restart_n: keeperRestarts.length,
		});
		getMultiplexedPool().restartKeeper();
	}

	sessionMgr.setOnKeeperDegraded(() => {
		if (reconcileInFlight) {
			pendingDegradedRemediation = true;
			log.info("worker", "keeper_degraded_reconcile_inflight", {});
			return;
		}
		if (!reconcileAdmitted) {
			pendingDegradedRemediation = true;
			log.info("worker", "keeper_degraded_reconcile_retry", {});
			void reconcileOpenSessions("keeper_degraded").catch((error) => {
				queueMicrotask(() => {
					throw error;
				});
			});
			return;
		}
		remediateDegradedKeeper();
	});

	return { reconcileOpenSessions, acquireKeeperUpdateBoundary };
}
