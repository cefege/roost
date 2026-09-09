// Coordinator session reconciliation reserves every durable session outcome
// before keeper or SessionManager mutation. boot-reconcile serializes calls;
// failed adoption restores only after replacement-shell admission returns.

import type {
	SessionRecoveryMetadata as SessionRecoveryMetadataProto,
} from "@roost/shared/proto/coordinator_pb";
import {
	sessionRecoveryMetadataFromProto,
} from "@roost/shared/agent-conversation-reference-proto";
import type {
	AgentConversationReferenceV1,
} from "@roost/shared/agent-conversation-reference";
import { log } from "@roost/shared/log";
import type { WorkerFp } from "@roost/shared/wire";
import type { CoordClient } from "./coord-client.ts";
import type { SessionEventReservation } from "./event-sink.ts";
import {
	conversationRestoreDedupeKey,
	type AgentConversationRestoreOutcome,
} from "./agent-conversation-restore.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import {
	isSessionEventOutboxFullError,
	isSessionEventDurabilityError,
	type SessionManager,
} from "./session-manager.ts";
import { resolveShellSpec, type ShellSpec } from "./shell-spec.ts";
import { withAgentStatusEnvironment } from "./agent-status/environment.ts";
import { isTerminalCoreCapacityError } from "./terminal-core-capacity.ts";

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
	referenceRecoveryAdmission: <Result>(
		read: () => Promise<Result>,
	) => Promise<Result>;
	prepareKeeper: (
		coordinatorOpenSessionIds: ReadonlySet<string>,
	) => Promise<void>;
	restoreAgentConversation: (
		sessionId: string,
		reference: AgentConversationReferenceV1 | null,
		resumedReferenceKeys: Set<string>,
	) => Promise<AgentConversationRestoreOutcome>;
	onReconciled?: (reconciledAtMs: number) => void;
}

export async function reconcileCoordinatorSessions(
	deps: CoordinatorSessionReconcileDeps,
	reason: string,
): Promise<ReconcileAdmissionOutcome> {
	const {
		client,
		workerFp,
		sessionMgr,
		prepareKeeper,
		referenceRecoveryAdmission,
		restoreAgentConversation,
		onReconciled,
	} = deps;
	try {
		const recovery = await referenceRecoveryAdmission(async () => {
			const response = await client().sessionsList(
				{ workerFp, status: "open" },
				{ timeoutMs: BOOT_SESSION_ADMISSION_TIMEOUT_MS },
			);
			const referencesBySessionId = _assertExactRecoveryMetadata(
				response.sessions.map((session) => String(session.id)),
				response.recoveryMetadata,
			);
			return { response, referencesBySessionId };
		});
		const response = recovery.response;
		const shellRows = response.sessions;
		const coordinatorOpenSessionIds = new Set(
			shellRows.map((session) => String(session.id)),
		);
		const admissions: Array<{
			session: (typeof shellRows)[number];
			shellSpec: ShellSpec;
			agentReference: AgentConversationReferenceV1 | null;
			resumeClose: SessionEventReservation;
			respawnEvent: SessionEventReservation;
			futureClose: SessionEventReservation;
			resumeCloseOwned: boolean;
			respawnEventOwned: boolean;
			futureCloseOwned: boolean;
		}> = [];

		// No keeper or SessionManager state can change until capacity exists
		// for every durable path in the coordinator's complete open set.
		try {
			for (const session of shellRows) {
				const sessionId = String(session.id);
				const agentReference =
					recovery.referencesBySessionId.get(sessionId) ?? null;
				const shellSpec = resolveShellSpec({
					cwd: session.cwd,
					sessionId,
					envOverlay: withAgentStatusEnvironment({}, sessionId),
				});
				let resumeClose: SessionEventReservation | null = null;
				let respawnEvent: SessionEventReservation | null = null;
				let futureClose: SessionEventReservation | null = null;
				try {
					resumeClose = sessionMgr.reserveSessionEvent("closed");
					respawnEvent =
						sessionMgr.reserveSessionEvent("respawned");
					futureClose = sessionMgr.reserveSessionEvent("closed");
				} catch (error) {
					if (futureClose) {
						sessionMgr.releaseSessionEvent(futureClose);
					}
					if (respawnEvent) {
						sessionMgr.releaseSessionEvent(respawnEvent);
					}
					if (resumeClose) {
						sessionMgr.releaseSessionEvent(resumeClose);
					}
					throw error;
				}
				admissions.push({
					session,
					agentReference,
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
				sessionMgr.releaseSessionEvent(admission.futureClose);
				sessionMgr.releaseSessionEvent(admission.respawnEvent);
				sessionMgr.releaseSessionEvent(admission.resumeClose);
				admission.futureCloseOwned = false;
				admission.respawnEventOwned = false;
				admission.resumeCloseOwned = false;
			}
			throw error;
		}

		let resumed = 0;
		let respawned = 0;
		let respawnFailed = 0;
		const resumedReferenceKeys = new Set<string>();
		try {
			// Survivor retirement, keeper creation, and periodic reaping are
			// all downstream of the complete session-event reservation batch.
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
					// The adopted PTY still runs an agent on this reference,
					// so no other session may resume the same conversation.
					if (admission.agentReference) {
						resumedReferenceKeys.add(
							conversationRestoreDedupeKey(
								admission.agentReference,
							),
						);
					}
					sessionMgr.releaseSessionEvent(
						admission.respawnEvent,
					);
					admission.respawnEventOwned = false;
					sessionMgr.releaseSessionEvent(
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
						if (isSessionEventDurabilityError(error)) {
							throw error;
						}
						if (isTerminalCoreCapacityError(error)) {
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
							sessionMgr.releaseSessionEvent(
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
					try {
						await restoreAgentConversation(
							String(admission.session.id),
							admission.agentReference,
							resumedReferenceKeys,
						);
					} catch {
						log.warn(
							"worker",
							"agent_conversation_restore_transition",
							{
								sessionId: admission.session.id,
								outcome: "ambiguous",
							},
						);
					}
				} else {
					if (admission.respawnEventOwned) {
						sessionMgr.releaseSessionEvent(
							admission.respawnEvent,
						);
						admission.respawnEventOwned = false;
					}
					if (admission.futureCloseOwned) {
						sessionMgr.releaseSessionEvent(
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
					sessionMgr.releaseSessionEvent(
						admission.futureClose,
					);
				}
				if (admission.respawnEventOwned) {
					sessionMgr.releaseSessionEvent(
						admission.respawnEvent,
					);
				}
				if (admission.resumeCloseOwned) {
					sessionMgr.releaseSessionEvent(
						admission.resumeClose,
					);
				}
			}
		}
	} catch (error) {
		if (isSessionEventDurabilityError(error)) throw error;
		log.warn("worker", "resume_failed", {
			reason,
			error: isSessionEventOutboxFullError(error)
				? "session event outbox full"
				: String(error),
		});
		return { admitted: false, error };
	}
}

export function _assertExactRecoveryMetadata(
	sessionIds: readonly string[],
	rows: readonly SessionRecoveryMetadataProto[],
): ReadonlyMap<string, AgentConversationReferenceV1 | null> {
	if (rows.length !== sessionIds.length) {
		throw new Error("coordinator recovery metadata set is incomplete");
	}
	const expected = new Set(sessionIds);
	const seen = new Set<string>();
	const references = new Map<string, AgentConversationReferenceV1 | null>();
	for (const row of rows) {
		const sessionId = String(row.sessionId);
		let reference: AgentConversationReferenceV1 | null = null;
		try {
			reference = sessionRecoveryMetadataFromProto(row).agent_reference;
		} catch {
			// A stored reference that no longer satisfies the bounded contract
			// restores an ordinary shell. Failing admission here would exit the
			// worker and crash-loop it for every session on the machine.
			log.warn("worker", "recovery_reference_unusable", { session_id: sessionId });
		}
		if (!expected.has(sessionId) || seen.has(sessionId)) {
			throw new Error("coordinator recovery metadata set does not match sessions");
		}
		seen.add(sessionId);
		references.set(sessionId, reference);
	}
	if (seen.size !== expected.size) {
		throw new Error("coordinator recovery metadata set is incomplete");
	}
	return references;
}
