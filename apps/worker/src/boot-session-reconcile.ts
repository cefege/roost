// Coordinator session reconciliation reserves every durable lifecycle outcome
// before keeper or SessionManager mutation. boot-reconcile serializes calls
// and supplies keeper preparation plus the successful-admission timestamp hook.

import { log } from "@roost/shared/log";
import type { WorkerFp } from "@roost/shared/wire";
import type { CoordClient } from "./coord-client.ts";
import type { LifecycleReservation } from "./event-sink.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import {
	isLifecycleOutboxFullError,
	isSessionLifecycleDurabilityError,
	type SessionManager,
} from "./session-manager.ts";
import { resolveShellSpec, type ShellSpec } from "./shell-spec.ts";
import { withAgentStatusEnvironment } from "./agent-status/environment.ts";

const BOOT_SESSION_ADMISSION_TIMEOUT_MS = 10_000;

export interface ReconcileAdmissionSuccess {
	admitted: true;
	candidates: number;
	resumed: number;
	respawned: number;
	straysReaped: number;
}

export interface ReconcileAdmissionFailure {
	admitted: false;
	error: unknown;
}

export type ReconcileAdmissionOutcome =
	| ReconcileAdmissionSuccess
	| ReconcileAdmissionFailure;

export interface CoordinatorSessionReconcileDeps {
	client: () => CoordClient;
	workerFp: WorkerFp;
	sessionMgr: SessionManager;
	prepareKeeper: (
		coordinatorOpenSessionIds: ReadonlySet<string>,
	) => Promise<void>;
	onReconciled?: (reconciledAtMs: number) => void;
}

export async function reconcileCoordinatorSessions(
	deps: CoordinatorSessionReconcileDeps,
	reason: string,
): Promise<ReconcileAdmissionOutcome> {
	const { client, workerFp, sessionMgr, prepareKeeper, onReconciled } = deps;
	try {
		const response = await client().sessionsList(
			{ workerFp, status: "open" },
			{ timeoutMs: BOOT_SESSION_ADMISSION_TIMEOUT_MS },
		);
		const shellRows = response.sessions;
		const coordinatorOpenSessionIds = new Set(
			shellRows.map((session) => String(session.id)),
		);
		const admissions: Array<{
			session: (typeof shellRows)[number];
			shellSpec: ShellSpec;
			resumeClose: LifecycleReservation;
			respawnEvent: LifecycleReservation;
			futureClose: LifecycleReservation;
			resumeCloseOwned: boolean;
			respawnEventOwned: boolean;
			futureCloseOwned: boolean;
		}> = [];

		// No keeper or SessionManager state can change until capacity exists
		// for every lifecycle path in the coordinator's complete open set.
		try {
			for (const session of shellRows) {
				const shellSpec = resolveShellSpec({
					cwd: session.cwd,
					sessionId: String(session.id),
					envOverlay: withAgentStatusEnvironment(
						{},
						String(session.id),
					),
				});
				let resumeClose: LifecycleReservation | null = null;
				let respawnEvent: LifecycleReservation | null = null;
				let futureClose: LifecycleReservation | null = null;
				try {
					resumeClose = sessionMgr.reserveLifecycleEvent("closed");
					respawnEvent =
						sessionMgr.reserveLifecycleEvent("respawned");
					futureClose = sessionMgr.reserveLifecycleEvent("closed");
				} catch (error) {
					if (futureClose) {
						sessionMgr.releaseLifecycleEvent(futureClose);
					}
					if (respawnEvent) {
						sessionMgr.releaseLifecycleEvent(respawnEvent);
					}
					if (resumeClose) {
						sessionMgr.releaseLifecycleEvent(resumeClose);
					}
					throw error;
				}
				admissions.push({
					session,
					shellSpec,
					resumeClose,
					respawnEvent,
					futureClose,
					resumeCloseOwned: true,
					respawnEventOwned: true,
					futureCloseOwned: true,
				});
			}
		} catch (error) {
			for (const admission of admissions) {
				sessionMgr.releaseLifecycleEvent(admission.futureClose);
				sessionMgr.releaseLifecycleEvent(admission.respawnEvent);
				sessionMgr.releaseLifecycleEvent(admission.resumeClose);
				admission.futureCloseOwned = false;
				admission.respawnEventOwned = false;
				admission.resumeCloseOwned = false;
			}
			throw error;
		}

		let resumed = 0;
		let respawned = 0;
		let respawnFailed = 0;
		try {
			// Survivor retirement, keeper creation, and periodic reaping are
			// all downstream of the complete lifecycle reservation batch.
			await prepareKeeper(coordinatorOpenSessionIds);
			await sessionMgr.startPostAdmissionMaintenance();
			await sessionMgr.advanceChannelCounterPastKeeper();

			for (const admission of admissions) {
				admission.resumeCloseOwned = false;
				const didResume = await sessionMgr.resume({
					sessionId: admission.session.id as never,
					channelId: admission.session.channel as never,
					kind: admission.session.kind as never,
					cwd: admission.shellSpec.cwd,
					shellSpec: admission.shellSpec,
				}, admission.resumeClose);
				if (didResume) {
					resumed++;
					sessionMgr.releaseLifecycleEvent(
						admission.respawnEvent,
					);
					admission.respawnEventOwned = false;
					sessionMgr.releaseLifecycleEvent(
						admission.futureClose,
					);
					admission.futureCloseOwned = false;
					continue;
				}

				const respawnArgs = {
					oldSessionId: admission.session.id as never,
					cwd: admission.session.cwd,
					kind: "shell" as const,
					shellSpec: admission.shellSpec,
				};
				let ok = false;
				for (let attempt = 1; attempt <= 3; attempt++) {
					try {
						await sessionMgr.respawn(respawnArgs, {
							event: admission.respawnEvent,
							close: admission.futureClose,
						});
						admission.respawnEventOwned = false;
						admission.futureCloseOwned = false;
						ok = true;
						break;
					} catch (error) {
						if (isSessionLifecycleDurabilityError(error)) {
							throw error;
						}
						const errorText = String(error);
						const transient =
							/socket closed|not connected|ENOTCONN|not ready|timeout|SpawnErr|keeper/i.test(
								errorText,
							);
						if (attempt < 3 && transient) {
							log.info("worker", "respawn_retry_transient", {
								sessionId: admission.session.id,
								attempt,
								error: errorText,
							});
							try {
								await getMultiplexedPool().ensure();
							} catch {
								/* the bounded retry reports the final failure */
							}
							const { promise, resolve } =
								Promise.withResolvers<void>();
							setTimeout(resolve, attempt * 400);
							await promise;
							continue;
						}
						log.warn("worker", "respawn_failed", {
							sessionId: admission.session.id,
							cwd: admission.session.cwd,
							error: errorText,
							transient,
							after_retry: attempt > 1,
						});
						if (!transient) {
							sessionMgr.releaseLifecycleEvent(
								admission.respawnEvent,
							);
							admission.respawnEventOwned = false;
							admission.futureCloseOwned = false;
							sessionMgr.emitClosedTombstone(
								admission.session.id as never,
								admission.futureClose,
							);
						}
						respawnFailed++;
						break;
					}
				}
				if (ok) {
					respawned++;
				} else {
					if (admission.respawnEventOwned) {
						sessionMgr.releaseLifecycleEvent(
							admission.respawnEvent,
						);
						admission.respawnEventOwned = false;
					}
					if (admission.futureCloseOwned) {
						sessionMgr.releaseLifecycleEvent(
							admission.futureClose,
						);
						admission.futureCloseOwned = false;
					}
				}
			}

			if (respawnFailed > 0) {
				throw new Error(
					`reconcile left ${respawnFailed} coordinator session(s) unresolved`,
				);
			}
			const straysReaped =
				await sessionMgr.reapStrayKeeperChannels();
			const reconciledAtMs = Date.now();
			onReconciled?.(reconciledAtMs);
			log.info("worker", "resume_attempted", {
				reason,
				candidates: shellRows.length,
				resumed,
				respawned,
				respawn_failed: 0,
				strays_reaped: straysReaped,
			});
			return {
				admitted: true,
				candidates: shellRows.length,
				resumed,
				respawned,
				straysReaped,
			};
		} finally {
			for (const admission of admissions) {
				if (admission.futureCloseOwned) {
					sessionMgr.releaseLifecycleEvent(
						admission.futureClose,
					);
				}
				if (admission.respawnEventOwned) {
					sessionMgr.releaseLifecycleEvent(
						admission.respawnEvent,
					);
				}
				if (admission.resumeCloseOwned) {
					sessionMgr.releaseLifecycleEvent(
						admission.resumeClose,
					);
				}
			}
		}
	} catch (error) {
		if (isSessionLifecycleDurabilityError(error)) throw error;
		log.warn("worker", "resume_failed", {
			reason,
			error: isLifecycleOutboxFullError(error)
				? "session lifecycle outbox full"
				: String(error),
		});
		return { admitted: false, error };
	}
}
