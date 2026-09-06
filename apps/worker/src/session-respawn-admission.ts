// Session respawn admission owns collision checks and durable event reservations.
// SessionManager's shared channel-creation gate fences every delegated respawn
// against keeper replacement before this module can reach an asynchronous step.

import type { SessionId } from "@roost/shared/wire";
import type { LifecycleReservation } from "./event-sink.ts";
import type { SessionManager } from "./session-manager.ts";
import type { SessionRecord } from "./session-record.ts";
import * as respawnFns from "./session-respawn.ts";
import type { ShellSpec } from "./shell-spec.ts";

export interface SessionRespawnOptions {
	oldSessionId: SessionId;
	cwd: string;
	kind: "shell";
	cols?: number;
	rows?: number;
	shellSpec?: ShellSpec;
}

export interface SessionRespawnReservations {
	event: LifecycleReservation;
	close: LifecycleReservation;
}

export async function respawnIfMissing(
	this: SessionManager,
	pendingSpawnSessionIds: Set<SessionId>,
	sessionId: SessionId,
	cwd: string,
	cols: number,
	rows: number,
): Promise<SessionRecord> {
	const existing = this.getBySessionId(sessionId);
	if (existing) return existing;
	if (pendingSpawnSessionIds.has(sessionId)) {
		throw new Error(`session ${sessionId} is already live or spawning`);
	}
	pendingSpawnSessionIds.add(sessionId);
	try {
		await this.respawn({
			oldSessionId: sessionId,
			cwd,
			kind: "shell",
			cols,
			rows,
		});
		const respawned = this.getBySessionId(sessionId);
		if (!respawned) {
			throw new Error(`respawned session ${sessionId} is not live`);
		}
		return respawned;
	} finally {
		pendingSpawnSessionIds.delete(sessionId);
	}
}

export function respawn(
	this: SessionManager,
	opts: SessionRespawnOptions,
	reservations?: SessionRespawnReservations,
): Promise<void> {
	if (reservations) {
		return respawnFns.respawn.call(
			this,
			opts,
			reservations.event,
			reservations.close,
			false,
		);
	}
	const eventReservation = this.reserveLifecycleEvent("respawned");
	let closeReservation: LifecycleReservation;
	try {
		closeReservation = this.reserveLifecycleEvent("closed");
	} catch (error) {
		this.releaseLifecycleEvent(eventReservation);
		throw error;
	}
	return respawnFns.respawn.call(
		this,
		opts,
		eventReservation,
		closeReservation,
		true,
	);
}
