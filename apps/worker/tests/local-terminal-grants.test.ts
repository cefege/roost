// Grant-store lifecycle coverage: renewal keeps one grant identity, scope growth
// stays live, scope reduction is surfaced for carrier closure, and expiry actively
// removes the public scope. The test never observes a credential digest.

import { create } from "@bufbuild/protobuf";
import { expect, test } from "bun:test";
import { createHash, randomUUID } from "node:crypto";
import { DLocalTerminalGrantSchema } from "@roost/shared/proto/worker_transport_pb";
import {
	LocalTerminalGrantStore,
	type LocalTerminalGrantChange,
} from "../src/local-terminal-grants.ts";

const DEVICE = "a".repeat(64);
const TAB = "grant-tab";
const WORKER_EPOCH = "11111111-1111-4111-8111-111111111111";
const SESSION_A = "11111111-1111-4111-8111-111111111111";
const SESSION_B = "22222222-2222-4222-8222-222222222222";

interface ScheduledExpiry {
	callback: () => void;
	active: boolean;
}

test("renewal grows in place, reports scope reduction, and actively expires", () => {
	let now = 10_000;
	const scheduled: ScheduledExpiry[] = [];
	const store = new LocalTerminalGrantStore({
		workerEpoch: WORKER_EPOCH,
		now: () => now,
		scheduleTimeout: (callback) => {
			const entry = { callback, active: true };
			scheduled.push(entry);
			return entry as unknown as NodeJS.Timeout;
		},
		clearTimeout: (timer) => {
			(timer as unknown as ScheduledExpiry).active = false;
		},
	});
	const changes: LocalTerminalGrantChange[] = [];
	store.subscribe((change) => changes.push(change));
	const grantId = randomUUID();
	const secret = "direct-grant-secret";
	const install = (sessionIds: readonly string[], ttlMs: number) => store.install(create(DLocalTerminalGrantSchema, {
		requestId: randomUUID(),
		grantId,
		secretSha256: createHash("sha256").update(secret).digest("hex"),
		sessionIds: [...sessionIds],
		deviceFingerprint: DEVICE,
		tabId: TAB,
		ttlMs,
		workerEpoch: WORKER_EPOCH,
	}));

	const initial = install([SESSION_A], 100);
	const grown = install([SESSION_A, SESSION_B], 100);
	const reduced = install([SESSION_A], 100);

	expect(initial.grantId).toBe(grown.grantId);
	expect(store.current(grantId)?.sessionIds).toEqual([SESSION_A]);
	expect(changes.map((change) => change.kind)).toEqual(["installed", "renewed", "renewed"]);
	expect(changes[1]).toMatchObject({ removedSessionIds: [] });
	expect(changes[2]).toMatchObject({ removedSessionIds: [SESSION_B] });
	expect("secretSha256" in reduced).toBe(false);

	now += 101;
	const expiry = scheduled.at(-1)!;
	expect(expiry.active).toBe(true);
	expiry.callback();

	expect(store.current(grantId)).toBeNull();
	expect(changes.at(-1)).toMatchObject({ kind: "removed", reason: "expired" });
	store.dispose();
});
