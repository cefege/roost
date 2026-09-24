// TerminalInputRouteOwner fences browser terminal writers at the worker boundary.
// Authenticated ingress claims an actor/session route before any bytes reach the
// keeper; direct ports consult isCurrent while SessionManager performs the final
// live-authority check immediately before its write.
import { create } from "@bufbuild/protobuf";
import {
	TerminalInputRouteResultSchema,
	type TerminalInputRouteClaim,
	type TerminalInputRouteResult,
} from "@roost/protocol/proto/sync_pb";
import { log } from "@roost/observability/log";
import { randomUUID } from "node:crypto";
import { acquireKeeperAdmission, type KeeperAdmissionTicket } from "./session-control-lanes.ts";
import type { SessionManager } from "./session-manager.ts";
import {
	type TerminalInputWorkBudget,
	type TerminalRouteClaimWorkReservation,
} from "./terminal-input-work-budget.ts";
import type { TerminalRequestBudget } from "./transport/coord-link-types.ts";
import { monoNowMs } from "./util/mono.ts";
const ROUTE_IDENTIFIER_MAX_BYTES = 128;
const MAX_ROUTE_REVISION = (1n << 63n) - 1n;
export const TERMINAL_INPUT_ROUTE_TOMBSTONE_MS = 60_000;
export const TERMINAL_INPUT_ROUTE_MAX_ENTRIES = 8_192;
export interface TerminalInputRouteActor {
	deviceFingerprint: string;
	tabId: string;
	connectionId: string;
}
/** The ingress retains this live predicate instead of snapshotting grant scope. */
export interface TerminalInputRouteClaimBudget extends TerminalRequestBudget {
	isSessionAuthorized(): boolean;
}
export interface TerminalInputRouteOwnerDeps {
	workerEpoch: string;
	sessions(): SessionManager;
	inputWorkBudget: TerminalInputWorkBudget;
	/** Test seam; production uses the worker monotonic clock. */
	now?(): number;
}

type RouteStatus = "active" | "blocked" | "retired";
interface RouteClaimIdentity {
	requestId: string;
	sessionId: string;
	revision: bigint;
}
interface RouteAttempt {
	routeKey: string;
	actor: TerminalInputRouteActor;
	command: TerminalInputRouteClaim;
	channelId: number;
	ticket: KeeperAdmissionTicket;
	reservation: TerminalRouteClaimWorkReservation;
	cancellation: Promise<void>;
	cancellationReason: string | null;
	cancel(reason: string): void;
	operation: Promise<TerminalInputRouteResult> | null;
}
interface RouteEntry {
	actor: TerminalInputRouteActor;
	sessionId: string;
	workerEpoch: string;
	latestRevision: bigint;
	inputRouteEpoch: string | null;
	status: RouteStatus;
	retiredUntilMonoMs: number | null;
	latestClaim: RouteClaimIdentity;
	latestResult: TerminalInputRouteResult | null;
	pending: RouteAttempt | null;
}
export class TerminalInputRouteOwner {
	private readonly routes = new Map<string, RouteEntry>();
	private readonly now: () => number;
	private disposed = false;
	private readonly revokedDevices = new Set<string>(); private revokeOverflow = false;

	constructor(private readonly deps: TerminalInputRouteOwnerDeps) {
		this.now = deps.now ?? monoNowMs;
	}

	async claim(
		actor: TerminalInputRouteActor,
		command: TerminalInputRouteClaim,
		budget: TerminalInputRouteClaimBudget,
	): Promise<TerminalInputRouteResult> {
		this.pruneRetired();
		if (!validIdentifier(actor.deviceFingerprint)
			|| !validIdentifier(actor.tabId)
			|| !validIdentifier(actor.connectionId)
			|| !validIdentifier(command.requestId)
			|| !validIdentifier(command.sessionId)
			|| !validIdentifier(command.workerEpoch)
			|| command.revision <= 0n
			|| command.revision > MAX_ROUTE_REVISION) {
			return this.result(command, false, 0n, "", "invalid_route_claim");
		}
		if (this.disposed) return this.result(command, false, 0n, "", "route_claim_busy");
		if (this.revokeOverflow || this.revokedDevices.has(actor.deviceFingerprint)) return this.result(command, false, 0n, "", "device_revoked");
		if (command.workerEpoch !== this.deps.workerEpoch) {
			return this.result(command, false, 0n, "", "worker_epoch_mismatch");
		}
		const key = routeKey(actor, command.sessionId);
		const existing = this.routes.get(key);
		if (existing) {
			const exactLatest = existing.latestClaim.requestId === command.requestId
				&& existing.latestClaim.revision === command.revision
				&& existing.actor.connectionId === actor.connectionId;
			if (exactLatest) {
				if (existing.pending?.operation) return existing.pending.operation;
				if (existing.latestResult) return existing.latestResult;
			}
			if (command.revision <= existing.latestRevision) {
				return this.result(command, false, existing.latestRevision, "", "stale_route_revision");
			}
		}
		const manager = this.deps.sessions();
		const record = manager.getBySessionId(command.sessionId);
		const preAdmissionFailure = this.preAdmissionFailure(record?.channelId, budget);
		if (preAdmissionFailure) {
			return this.result(command, false, existing?.latestRevision ?? 0n, "", preAdmissionFailure);
		}
		if (!existing && this.routes.size >= TERMINAL_INPUT_ROUTE_MAX_ENTRIES) {
			return this.result(command, false, 0n, "", "route_claim_busy");
		}
		const actorSessionKey = JSON.stringify([
			actor.deviceFingerprint,
			actor.tabId,
			actor.connectionId,
			command.sessionId,
		]);
		if (existing?.pending) {
			return this.result(command, false, existing.latestRevision, "", "route_claim_busy");
		}
		const workAdmission = this.deps.inputWorkBudget.reserveRouteClaim(
			actor.connectionId,
			actorSessionKey,
		);
		if (!workAdmission.admitted) {
			return this.result(command, false, existing?.latestRevision ?? 0n, "", workAdmission.reason);
		}
		const reservation = workAdmission.reservation;
		const admission = acquireKeeperAdmission(manager, record!.channelId, "terminal_input");
		if (!admission.admitted) {
			reservation.release();
			return this.result(command, false, existing?.latestRevision ?? 0n, "", admission.reason);
		}
		const ticket = admission.ticket;
		const entry: RouteEntry = existing ?? {
			actor: copyActor(actor),
			sessionId: command.sessionId,
			workerEpoch: this.deps.workerEpoch,
			latestRevision: 0n,
			inputRouteEpoch: null,
			status: "retired",
			retiredUntilMonoMs: null,
			latestClaim: claimIdentity(command),
			latestResult: null,
			pending: null,
		};
		entry.actor = copyActor(actor);
		entry.sessionId = command.sessionId;
		entry.workerEpoch = this.deps.workerEpoch;
		entry.latestRevision = command.revision;
		entry.inputRouteEpoch = null;
		entry.status = "blocked";
		entry.retiredUntilMonoMs = null;
		entry.latestClaim = claimIdentity(command);
		entry.latestResult = null;
		const cancellation = Promise.withResolvers<void>();
		let attempt!: RouteAttempt;
		attempt = {
			routeKey: key,
			actor: copyActor(actor),
			command,
			channelId: record!.channelId,
			ticket,
			reservation,
			cancellation: cancellation.promise,
			cancellationReason: null,
			cancel: (reason) => {
				if (attempt.cancellationReason !== null) return;
				attempt.cancellationReason = reason;
				attempt.ticket.release();
				// Admission tickets cannot be unlinked from the keeper lane. Keep
				// their claim capacity charged until this ticket actually drains.
				void attempt.ticket.granted.then(() => attempt.reservation.release());
				cancellation.resolve();
			},
			operation: null,
		};
		entry.pending = attempt;
		this.routes.set(key, entry);
		const operation = this.completeClaim(entry, attempt, budget);
		attempt.operation = operation;
		return operation;
	}

	isCurrent(actor: TerminalInputRouteActor, sessionId: string, epoch: string): boolean {
		if (!validIdentifier(actor.deviceFingerprint)
			|| !validIdentifier(actor.tabId)
			|| !validIdentifier(actor.connectionId)
			|| !validIdentifier(sessionId)
			|| epoch.length === 0) return false;
		if (this.revokeOverflow || this.revokedDevices.has(actor.deviceFingerprint)) return false;
		const entry = this.routes.get(routeKey(actor, sessionId));
		return entry?.status === "active"
			&& entry.workerEpoch === this.deps.workerEpoch
			&& entry.actor.connectionId === actor.connectionId
			&& entry.inputRouteEpoch === epoch;
	}

	allowsLegacyInput(actor: TerminalInputRouteActor, sessionId: string): boolean {
		if (!validIdentifier(actor.deviceFingerprint)
			|| !validIdentifier(actor.tabId)
			|| !validIdentifier(actor.connectionId)
			|| !validIdentifier(sessionId)) return false;
		if (this.revokeOverflow || this.revokedDevices.has(actor.deviceFingerprint)) return false;
		const key = routeKey(actor, sessionId);
		const entry = this.routes.get(key);
		if (!entry) return true;
		if (entry.status !== "retired" || entry.pending !== null
			|| entry.retiredUntilMonoMs === null || entry.retiredUntilMonoMs > this.now()) return false;
		this.routes.delete(key);
		return true;
	}

	retireConnection(connectionId: string): void {
		if (!validIdentifier(connectionId)) return;
		this.pruneRetired();
		for (const entry of this.routes.values()) {
			if (entry.actor.connectionId === connectionId || entry.pending?.actor.connectionId === connectionId) {
				this.retireEntry(entry);
			}
		}
	}

	retireSession(sessionId: string): void {
		if (!validIdentifier(sessionId)) return;
		this.pruneRetired();
		for (const entry of this.routes.values()) {
			if (entry.sessionId === sessionId) this.retireEntry(entry);
		}
	}

	revokeDevice(deviceFingerprint: string): void {
		if (!validIdentifier(deviceFingerprint)) return;
		if (this.revokedDevices.size >= TERMINAL_INPUT_ROUTE_MAX_ENTRIES) this.revokeOverflow = true;
		else this.revokedDevices.add(deviceFingerprint);
		this.pruneRetired();
		for (const entry of this.routes.values()) {
			if (entry.actor.deviceFingerprint === deviceFingerprint) this.retireEntry(entry);
		}
	}

	dispose(): void {
		if (this.disposed) return;
		this.disposed = true;
		for (const entry of this.routes.values()) entry.pending?.cancel("route_owner_disposed");
		this.routes.clear();
		this.revokedDevices.clear();
	}

	private async completeClaim(
		entry: RouteEntry,
		attempt: RouteAttempt,
		budget: TerminalInputRouteClaimBudget,
	): Promise<TerminalInputRouteResult> {
		try {
			const cancelled = await Promise.race([
				attempt.ticket.granted.then(() => false),
				attempt.cancellation.then(() => true),
			]);
			if (cancelled || !this.currentAttempt(entry, attempt)) {
				return this.result(
					attempt.command,
					false,
					entry.latestRevision,
					"",
					attempt.cancellationReason ?? "route_claim_retired",
				);
			}
			const currentRecord = this.deps.sessions().getBySessionId(attempt.command.sessionId);
			const failure = this.preAdmissionFailure(
				currentRecord?.channelId === attempt.channelId ? attempt.channelId : undefined,
				budget,
			);
			if (failure) return this.failAttempt(entry, attempt, failure);
			const inputRouteEpoch = randomUUID();
			entry.inputRouteEpoch = inputRouteEpoch;
			entry.status = "active";
			entry.retiredUntilMonoMs = null;
			entry.pending = null;
			const result = this.result(attempt.command, true, entry.latestRevision, inputRouteEpoch, "");
			entry.latestResult = result;
			log.info("worker", "terminal_input_route_active", { session_id: attempt.command.sessionId });
			return result;
		} finally {
			attempt.ticket.release();
			if (attempt.cancellationReason === null) attempt.reservation.release();
		}
	}

	private currentAttempt(entry: RouteEntry, attempt: RouteAttempt): boolean {
		return !this.disposed
			&& this.routes.get(attempt.routeKey) === entry
			&& entry.pending === attempt
			&& entry.status === "blocked"
			&& entry.latestRevision === attempt.command.revision
			&& entry.actor.connectionId === attempt.actor.connectionId;
	}

	private failAttempt(
		entry: RouteEntry,
		attempt: RouteAttempt,
		reason: string,
	): TerminalInputRouteResult {
		const result = this.result(attempt.command, false, entry.latestRevision, "", reason);
		if (!this.currentAttempt(entry, attempt)) return result;
		entry.inputRouteEpoch = null;
		entry.status = "retired";
		entry.retiredUntilMonoMs = this.now() + TERMINAL_INPUT_ROUTE_TOMBSTONE_MS;
		entry.pending = null;
		entry.latestResult = result;
		log.warn("worker", "terminal_input_route_retired", { session_id: attempt.command.sessionId });
		return result;
	}

	private retireEntry(entry: RouteEntry): void {
		const pending = entry.pending;
		pending?.cancel("route_retired");
		entry.inputRouteEpoch = null;
		entry.status = "retired";
		entry.retiredUntilMonoMs = this.now() + TERMINAL_INPUT_ROUTE_TOMBSTONE_MS;
		entry.latestResult = this.result(entry.latestClaim, false, entry.latestRevision, "", "route_retired");
		if (pending) {
			void pending.ticket.granted.then(() => {
				if (entry.pending === pending && entry.status === "retired") {
					entry.pending = null;
					this.pruneRetired();
				}
			});
		}
		log.info("worker", "terminal_input_route_retired", { session_id: entry.sessionId });
	}

	private preAdmissionFailure(
		channelId: number | undefined,
		budget: TerminalInputRouteClaimBudget,
	): string | null {
		if (channelId === undefined || !budget.isSessionAuthorized()) return "terminal session is unavailable";
		if (!budget.isCurrentConnection()) return "worker connection superseded";
		if (budget.remainingMs() <= 0) return "route claim budget expired";
		return null;
	}

	private pruneRetired(): void {
		const now = this.now();
		for (const [key, entry] of this.routes) {
			if (entry.status === "retired"
				&& entry.pending === null
				&& entry.retiredUntilMonoMs !== null
				&& entry.retiredUntilMonoMs <= now) this.routes.delete(key);
		}
	}

	private result(
		claim: RouteClaimIdentity,
		accepted: boolean,
		latestRevision: bigint,
		inputRouteEpoch: string,
		reason: string,
	): TerminalInputRouteResult {
		return create(TerminalInputRouteResultSchema, {
			requestId: claim.requestId,
			sessionId: claim.sessionId,
			revision: claim.revision,
			accepted,
			latestRevision,
			inputRouteEpoch,
			workerEpoch: this.deps.workerEpoch,
			reason,
		});
	}
}

function routeKey(actor: TerminalInputRouteActor, sessionId: string): string {
	return JSON.stringify([actor.deviceFingerprint, actor.tabId, sessionId]);
}
function claimIdentity(command: TerminalInputRouteClaim): RouteClaimIdentity {
	return { requestId: command.requestId, sessionId: command.sessionId, revision: command.revision };
}
function copyActor(actor: TerminalInputRouteActor): TerminalInputRouteActor {
	return { deviceFingerprint: actor.deviceFingerprint, tabId: actor.tabId, connectionId: actor.connectionId };
}
function validIdentifier(value: string): boolean {
	const byteLength = Buffer.byteLength(value, "utf8");
	return byteLength > 0 && byteLength <= ROUTE_IDENTIFIER_MAX_BYTES;
}
