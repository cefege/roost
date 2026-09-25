import { create } from "@bufbuild/protobuf";
import { afterEach, describe, expect, test } from "bun:test";
import {
	TerminalInputRouteClaimSchema,
	type TerminalInputRouteResult,
} from "@roost/protocol/proto/sync_pb";
import {
	TERMINAL_INPUT_ROUTE_TOMBSTONE_MS,
	TerminalInputRouteOwner,
	type TerminalInputRouteActor,
	type TerminalInputRouteClaimBudget,
} from "../../src/terminal/terminal-input-route-owner.ts";
import {
	TERMINAL_ROUTE_CLAIM_WORK_MAX_REQUESTS,
	TerminalInputWorkBudget,
} from "../../src/terminal/terminal-input-work-budget.ts";
import {
	CHANNEL_ID,
	cleanupStreamHarnesses,
	holdKeeperAdmission,
	makeHarness,
	SESSION_ID,
} from "./terminal-stream-state-harness.ts";

const WORKER_EPOCH = "worker-epoch";
const ACTOR: TerminalInputRouteActor = {
	deviceFingerprint: "device-fingerprint",
	tabId: "browser-tab",
	connectionId: "browser-connection",
};

interface ClaimLiveness {
	sessionAuthorized: boolean;
	currentConnection: boolean;
	remainingMs: number;
}

function routeClaim(revision: bigint, requestId: string) {
	return create(TerminalInputRouteClaimSchema, {
		requestId,
		sessionId: SESSION_ID,
		revision,
		domainGeneration: 0n,
		workerEpoch: WORKER_EPOCH,
	});
}

function claimBudget(liveness: ClaimLiveness): TerminalInputRouteClaimBudget {
	return {
		isSessionAuthorized: () => liveness.sessionAuthorized,
		isCurrentConnection: () => liveness.currentConnection,
		remainingMs: () => liveness.remainingMs,
	};
}

afterEach(cleanupStreamHarnesses);

describe("TerminalInputRouteOwner", () => {
	test("caches an exact claim while rejecting stale revisions and rotating epochs", async () => {
		const harness = await makeHarness();
		const liveness = { sessionAuthorized: true, currentConnection: true, remainingMs: 10_000 };
		const owner = new TerminalInputRouteOwner({
			workerEpoch: WORKER_EPOCH,
			sessions: () => harness.manager,
			inputWorkBudget: new TerminalInputWorkBudget(),
		});
		const firstCommand = routeClaim(1n, "claim-one");
		const first = await owner.claim(ACTOR, firstCommand, claimBudget(liveness));
		expect(first.accepted).toBe(true);
		expect(first.workerEpoch).toBe(WORKER_EPOCH);
		expect(owner.isCurrent(ACTOR, SESSION_ID, first.inputRouteEpoch)).toBe(true);
		const cached = await owner.claim(ACTOR, firstCommand, claimBudget(liveness));
		expect(cached).toBe(first);
		const stale = await owner.claim(ACTOR, routeClaim(1n, "claim-one-other"), claimBudget(liveness));
		expect(stale).toMatchObject({
			accepted: false,
			latestRevision: 1n,
			reason: "stale_route_revision",
		});
		const next = await owner.claim(ACTOR, routeClaim(2n, "claim-two"), claimBudget(liveness));
		expect(next.accepted).toBe(true);
		expect(next.inputRouteEpoch).not.toBe(first.inputRouteEpoch);
		expect(owner.isCurrent(ACTOR, SESSION_ID, first.inputRouteEpoch)).toBe(false);
		expect(owner.isCurrent(ACTOR, SESSION_ID, next.inputRouteEpoch)).toBe(true);
	});

	test("rejects out-of-range claim revisions and identifiers before keeper admission", async () => {
		const harness = await makeHarness();
		const liveness = { sessionAuthorized: true, currentConnection: true, remainingMs: 10_000 };
		const owner = new TerminalInputRouteOwner({
			workerEpoch: WORKER_EPOCH,
			sessions: () => harness.manager,
			inputWorkBudget: new TerminalInputWorkBudget(),
		});
		for (const command of [
			routeClaim(0n, "claim-zero"),
			routeClaim(1n << 63n, "claim-too-large"),
			routeClaim(1n, "x".repeat(129)),
		]) {
			expect(await owner.claim(ACTOR, command, claimBudget(liveness))).toMatchObject({
				accepted: false,
				reason: "invalid_route_claim",
			});
		}
		harness.manager.sessions.delete(CHANNEL_ID);
		expect(await owner.claim(ACTOR, routeClaim(1n, "claim-missing-session"), claimBudget(liveness)))
			.toMatchObject({ accepted: false, reason: "terminal session is unavailable" });
	});

	test("does not restore a prior epoch after a queued claim loses live authority", async () => {
		const harness = await makeHarness();
		const liveness = { sessionAuthorized: true, currentConnection: true, remainingMs: 10_000 };
		const owner = new TerminalInputRouteOwner({
			workerEpoch: WORKER_EPOCH,
			sessions: () => harness.manager,
			inputWorkBudget: new TerminalInputWorkBudget(),
		});
		const initial = await owner.claim(ACTOR, routeClaim(1n, "claim-before-loss"), claimBudget(liveness));
		const blocker = holdKeeperAdmission(harness.manager, CHANNEL_ID, "terminal_resize");
		await blocker.granted;
		const replacement = owner.claim(ACTOR, routeClaim(2n, "claim-after-loss"), claimBudget(liveness));
		expect(owner.isCurrent(ACTOR, SESSION_ID, initial.inputRouteEpoch)).toBe(false);
		liveness.sessionAuthorized = false;
		blocker.release();
		expect(await replacement).toMatchObject({
			accepted: false,
			reason: "terminal session is unavailable",
		});
		expect(owner.isCurrent(ACTOR, SESSION_ID, initial.inputRouteEpoch)).toBe(false);
	});

	test("refuses repeated newer claims while one keeper ticket is pending", async () => {
		const harness = await makeHarness();
		const liveness = { sessionAuthorized: true, currentConnection: true, remainingMs: 10_000 };
		const owner = new TerminalInputRouteOwner({
			workerEpoch: WORKER_EPOCH,
			sessions: () => harness.manager,
			inputWorkBudget: new TerminalInputWorkBudget(),
		});
		const initial = await owner.claim(ACTOR, routeClaim(1n, "claim-initial"), claimBudget(liveness));
		const blocker = holdKeeperAdmission(harness.manager, CHANNEL_ID, "terminal_resize");
		await blocker.granted;
		const pending = owner.claim(ACTOR, routeClaim(2n, "claim-held"), claimBudget(liveness));
		expect(owner.isCurrent(ACTOR, SESSION_ID, initial.inputRouteEpoch)).toBe(false);
		for (let revision = 3n; revision <= 64n; revision += 1n) {
			const refused = await owner.claim(
				ACTOR,
				routeClaim(revision, `claim-refused-${revision}`),
				claimBudget(liveness),
			);
			expect(refused).toMatchObject({ accepted: false, reason: "route_claim_busy" });
		}
		expect(harness.manager.keeperAdmissionLane.get(CHANNEL_ID)).toMatchObject({
			depth: 1,
			holder: "terminal_resize",
		});
		blocker.release();
		const accepted = await pending;
		expect(accepted).toMatchObject({ accepted: true, revision: 2n });
		expect(owner.isCurrent(ACTOR, SESSION_ID, accepted.inputRouteEpoch)).toBe(true);
	});

	test("rejects excess pending claims without disturbing an active route", async () => {
		const harness = await makeHarness();
		const liveness = { sessionAuthorized: true, currentConnection: true, remainingMs: 10_000 };
		const owner = new TerminalInputRouteOwner({
			workerEpoch: WORKER_EPOCH,
			sessions: () => harness.manager,
			inputWorkBudget: new TerminalInputWorkBudget(),
		});
		const active = await owner.claim(ACTOR, routeClaim(1n, "claim-active"), claimBudget(liveness));
		const blocker = holdKeeperAdmission(harness.manager, CHANNEL_ID, "terminal_resize");
		await blocker.granted;
		const pending: Promise<TerminalInputRouteResult>[] = [];
		for (let index = 0; index < 4; index += 1) {
			pending.push(owner.claim({
				deviceFingerprint: `device-${index}`,
				tabId: `tab-${index}`,
				connectionId: ACTOR.connectionId,
			}, routeClaim(1n, `claim-pending-${index}`), claimBudget(liveness)));
		}
		const refused = await owner.claim(
			ACTOR,
			routeClaim(2n, "claim-pending-overflow"),
			claimBudget(liveness),
		);
		expect(refused).toMatchObject({ accepted: false, reason: "route_claim_busy" });
		expect(owner.isCurrent(ACTOR, SESSION_ID, active.inputRouteEpoch)).toBe(true);
		blocker.release();
		for (const result of await Promise.all(pending)) expect(result.accepted).toBe(true);
	});

	test("keeps retired claim capacity charged until queued tickets drain", async () => {
		const harness = await makeHarness();
		const liveness = { sessionAuthorized: true, currentConnection: true, remainingMs: 10_000 };
		const owner = new TerminalInputRouteOwner({
			workerEpoch: WORKER_EPOCH,
			sessions: () => harness.manager,
			inputWorkBudget: new TerminalInputWorkBudget(),
		});
		const blocker = holdKeeperAdmission(harness.manager, CHANNEL_ID, "terminal_resize");
		await blocker.granted;
		const retired: Promise<TerminalInputRouteResult>[] = [];
		for (let index = 0; index < TERMINAL_ROUTE_CLAIM_WORK_MAX_REQUESTS; index += 1) {
			const actor = {
				deviceFingerprint: `retired-device-${index}`,
				tabId: `retired-tab-${index}`,
				connectionId: `retired-connection-${index}`,
			};
			retired.push(owner.claim(actor, routeClaim(1n, `retired-claim-${index}`), claimBudget(liveness)));
			owner.retireConnection(actor.connectionId);
		}
		const overflow = await owner.claim({
			deviceFingerprint: "retired-device-overflow",
			tabId: "retired-tab-overflow",
			connectionId: "retired-connection-overflow",
		}, routeClaim(1n, "retired-claim-overflow"), claimBudget(liveness));
		expect(overflow).toMatchObject({ accepted: false, reason: "route_claim_busy" });
		const queuedLane = harness.manager.keeperAdmissionLane.get(CHANNEL_ID);
		expect(queuedLane).toMatchObject({ depth: TERMINAL_ROUTE_CLAIM_WORK_MAX_REQUESTS });
		blocker.release();
		await queuedLane!.tail;
		await Promise.resolve();
		for (const result of await Promise.all(retired)) {
			expect(result).toMatchObject({ accepted: false, reason: "route_retired" });
		}
	});

	test("retains a tombstone through its revision fence and prunes it after its lease", async () => {
		const harness = await makeHarness();
		const liveness = { sessionAuthorized: true, currentConnection: true, remainingMs: 10_000 };
		let now = 1_000;
		const owner = new TerminalInputRouteOwner({
			workerEpoch: WORKER_EPOCH,
			sessions: () => harness.manager,
			inputWorkBudget: new TerminalInputWorkBudget(),
			now: () => now,
		});
		const active = await owner.claim(ACTOR, routeClaim(9n, "claim-tombstone"), claimBudget(liveness));
		owner.retireConnection(ACTOR.connectionId);
		expect(owner.isCurrent(ACTOR, SESSION_ID, active.inputRouteEpoch)).toBe(false);
		expect(owner.allowsLegacyInput(ACTOR, SESSION_ID)).toBe(false);
		const stale = await owner.claim(ACTOR, routeClaim(9n, "claim-tombstone-other"), claimBudget(liveness));
		expect(stale).toMatchObject({
			accepted: false,
			latestRevision: 9n,
			reason: "stale_route_revision",
		});
		now += TERMINAL_INPUT_ROUTE_TOMBSTONE_MS + 1;
		expect(owner.allowsLegacyInput(ACTOR, SESSION_ID)).toBe(true);
		const afterPrune = await owner.claim(ACTOR, routeClaim(1n, "claim-after-prune"), claimBudget(liveness));
		expect(afterPrune).toMatchObject({ accepted: true, revision: 1n });
		owner.revokeDevice(ACTOR.deviceFingerprint);
		expect(owner.isCurrent(ACTOR, SESSION_ID, afterPrune.inputRouteEpoch)).toBe(false);
		expect(owner.allowsLegacyInput(ACTOR, SESSION_ID)).toBe(false);
		expect(owner.allowsLegacyInput({
			...ACTOR,
			tabId: "revoked-device-other-tab",
			connectionId: "revoked-device-other-connection",
		}, SESSION_ID)).toBe(false);
	});
});
