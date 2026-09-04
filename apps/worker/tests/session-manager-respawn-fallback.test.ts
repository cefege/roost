// Exercises the coordinator fallback through the real SessionManager and keeper.
// The logical session already exists upstream, so replacement must reserve and
// publish respawn lifecycle state without creating a second logical session.

import { afterAll, afterEach, describe, expect, test } from "bun:test";
import { randomUUID } from "node:crypto";
import { asSessionId, asWorkerFp } from "@roost/shared/wire";
import type { DurableLifecycleKind } from "../src/event-sink.ts";
import { getMultiplexedPool } from "../src/keeper/multiplexed-client.ts";
import { SessionManager } from "../src/session-manager.ts";
import { LifecycleTestSink } from "./lifecycle-test-sink.ts";

process.env.ROOST_KEEPER_QUIET = "1";

class RecordingLifecycleSink extends LifecycleTestSink {
	readonly reservationKinds: DurableLifecycleKind[] = [];

	override reserveLifecycleEvent(kind: DurableLifecycleKind) {
		this.reservationKinds.push(kind);
		return super.reserveLifecycleEvent(kind);
	}
}

const pool = getMultiplexedPool();
const managers: SessionManager[] = [];

afterEach(() => {
	for (const manager of managers.splice(0)) {
		for (const record of manager.allSessions()) manager.kill(record.channelId);
		manager.dispose();
	}
});

afterAll(() => {
	const keeperPid = pool._keeperProc?.pid;
	pool.dispose();
	if (keeperPid) {
		try {
			process.kill(keeperPid, "SIGKILL");
		} catch {
			// The keeper may have completed shutdown after its final channel closed.
		}
	}
});

describe("SessionManager respawn-if-missing fallback", () => {
	test("reserves before keeper spawn and durably respawns the existing logical session", async () => {
		const sink = new RecordingLifecycleSink();
		const manager = new SessionManager({
			workerFp: asWorkerFp("42".repeat(32)),
			sink,
		});
		managers.push(manager);

		const priorChannel = manager.nextChannelId();
		const sessionId = asSessionId(randomUUID());
		const originalSpawn = pool.spawn;
		let spawnCalls = 0;
		let reservationKindsAtSpawn: DurableLifecycleKind[] = [];
		let lifecycleEventsAtSpawn = 0;
		pool.spawn = async function (options) {
			spawnCalls += 1;
			reservationKindsAtSpawn = [...sink.reservationKinds];
			lifecycleEventsAtSpawn = sink.events.filter((event) =>
				event.kind === "opened" || event.kind === "closed" || event.kind === "respawned"
			).length;
			return originalSpawn.call(this, options);
		};

		try {
			const record = await manager.respawnIfMissing(sessionId, process.cwd(), 91, 37);
			const lifecycleEvents = sink.events.filter((event) =>
				event.kind === "opened" || event.kind === "closed" || event.kind === "respawned"
			);

			expect(reservationKindsAtSpawn).toEqual(["respawned", "closed"]);
			expect(lifecycleEventsAtSpawn).toBe(0);
			expect(lifecycleEvents).toEqual([
				expect.objectContaining({
					kind: "respawned",
					session_id: sessionId,
					new_channel: record.channelId,
				}),
			]);
			expect(sink.events.some((event) => event.kind === "opened")).toBe(false);
			expect(record.sessionId).toBe(sessionId);
			expect(record.channelId).not.toBe(priorChannel);
			expect(manager.getBySessionId(sessionId)).toBe(record);
			expect(sink.active.size).toBe(1);

			const alreadyLive = await manager.respawnIfMissing(sessionId, "/ignored", 12, 8);
			expect(alreadyLive).toBe(record);
			expect(spawnCalls).toBe(1);
			expect(sink.reservationKinds).toEqual(["respawned", "closed"]);
			expect(sink.events.filter((event) => event.kind === "respawned")).toHaveLength(1);
		} finally {
			pool.spawn = originalSpawn;
		}
	}, 30_000);
});
