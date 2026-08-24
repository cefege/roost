// Pins the reconnect ladder's contract with the shared backoffDelayMs: equal
// jitter must stay within [cap/2, cap] so fleet-wide redials spread across a
// half-window instead of synchronizing into retry waves, and jitter:"none"
// must reproduce the legacy 500ms→30s ×2 ladder byte-for-byte, since the
// escalation logic in coord-link-backoff-cap.test.ts is calibrated against it.
// Pure: every armed dial timer is cancelled before the next schedule, so no
// test waits on real backoff.

import { describe, expect, test } from "bun:test";
import { createCoordLinkReconnect } from "../src/transport/coord-link-reconnect.ts";
import type { CoordLinkState } from "../src/transport/coord-link-types.ts";
import {
	BACKOFF_INITIAL_MS,
	BACKOFF_MAX_MS,
} from "../src/transport/coord-link-constants.ts";

type ReconnectingState = Extract<CoordLinkState, { kind: "reconnecting" }>;

function makeLadder(jitterOpts: { jitter?: "equal" | "full" | "none"; rng?: () => number }): {
	scheduleOnce(): number;
	ladder: ReturnType<typeof createCoordLinkReconnect>;
} {
	const states: ReconnectingState[] = [];
	const ladder = createCoordLinkReconnect({
		isDisposed: () => false,
		dial: () => {},
		setState: (next) => {
			if (next.kind === "reconnecting") states.push(next);
		},
		...jitterOpts,
	});
	return {
		// Arms a REAL timer of up to 30s; cancelled immediately so the observed
		// delay is the only effect under test.
		scheduleOnce(): number {
			ladder.scheduleReconnect();
			const delay = states[states.length - 1]!.backoffMs;
			ladder.cancelPendingDial();
			return delay;
		},
		ladder,
	};
}

/** Deterministic LCG so the equal-jitter bound test covers the whole [0,1)
 *  range without Math.random flakiness. */
function makeRng(seed: number): () => number {
	let state = seed >>> 0;
	return () => {
		state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
		return state / 0x1_0000_0000;
	};
}

describe("coord-link reconnect jitter", () => {
	test("jitter:none reproduces the legacy deterministic ladder", () => {
		const { scheduleOnce } = makeLadder({ jitter: "none" });
		for (let attempt = 0; attempt < 10; attempt++) {
			const expected = Math.min(BACKOFF_INITIAL_MS * 2 ** attempt, BACKOFF_MAX_MS);
			expect(scheduleOnce()).toBe(expected);
		}
	});

	test("equal jitter keeps every delay within [cap/2, cap]", () => {
		const rng = makeRng(0x5eed);
		const { scheduleOnce } = makeLadder({ rng });
		for (let index = 0; index < 64; index++) {
			const capped = Math.min(BACKOFF_INITIAL_MS * 2 ** index, BACKOFF_MAX_MS);
			const delay = scheduleOnce();
			expect(delay).toBeGreaterThanOrEqual(Math.floor(capped / 2));
			expect(delay).toBeLessThanOrEqual(capped);
		}
	});

	test("equal jitter bounds are tight: rng extremes hit both edges", () => {
		const low = makeLadder({ rng: () => 0 });
		expect(low.scheduleOnce()).toBe(Math.floor(BACKOFF_INITIAL_MS / 2));
		const high = makeLadder({ rng: () => 0.999_999 });
		expect(high.scheduleOnce()).toBe(BACKOFF_INITIAL_MS - 1);
	});

	test("full jitter spreads across [0, cap)", () => {
		const rng = makeRng(0xbeef);
		const { scheduleOnce } = makeLadder({ jitter: "full", rng });
		for (let index = 0; index < 32; index++) {
			const capped = Math.min(BACKOFF_INITIAL_MS * 2 ** index, BACKOFF_MAX_MS);
			const delay = scheduleOnce();
			expect(delay).toBeGreaterThanOrEqual(0);
			expect(delay).toBeLessThan(capped);
		}
	});
});
