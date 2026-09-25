import { describe, expect, test } from "bun:test";
import {
	TERMINAL_DIRECT_INPUT_WORK_MAX_BYTES,
	TERMINAL_DIRECT_INPUT_WORK_MAX_REQUESTS,
	TERMINAL_INPUT_WORK_MAX_BYTES,
	TERMINAL_INPUT_WORK_MAX_REQUESTS,
	TERMINAL_ROUTE_CLAIM_WORK_MAX_REQUESTS,
	TerminalInputWorkBudget,
	type TerminalInputWorkReservation,
} from "../../src/terminal/terminal-input-work-budget.ts";

describe("TerminalInputWorkBudget", () => {
	test("reserves global input count and retained bytes until exact-once release", () => {
		const countBudget = new TerminalInputWorkBudget();
		const reservations: TerminalInputWorkReservation[] = [];
		for (let index = 0; index < TERMINAL_INPUT_WORK_MAX_REQUESTS; index += 1) {
			const admission = countBudget.reserveInput({ origin: "sync", byteLength: 1 });
			if (!admission.admitted) throw new Error("global input reservation unexpectedly refused");
			reservations.push(admission.reservation);
		}
		expect(countBudget.reserveInput({ origin: "sync", byteLength: 1 })).toEqual({
			admitted: false,
			reason: "worker input admission is full",
		});
		reservations[0]!.release();
		expect(countBudget.reserveInput({ origin: "sync", byteLength: 1 }).admitted).toBe(true);
		for (const reservation of reservations) reservation.release();

		const byteBudget = new TerminalInputWorkBudget();
		const full = byteBudget.reserveInput({
			origin: "sync",
			byteLength: TERMINAL_INPUT_WORK_MAX_BYTES,
		});
		if (!full.admitted) throw new Error("full byte reservation unexpectedly refused");
		expect(byteBudget.reserveInput({ origin: "sync", byteLength: 1 }).admitted).toBe(false);
		full.reservation.release();
		full.reservation.release();
		const replaced = byteBudget.reserveInput({
			origin: "sync",
			byteLength: TERMINAL_INPUT_WORK_MAX_BYTES,
		});
		if (!replaced.admitted) throw new Error("released byte reservation was not reusable");
		expect(byteBudget.reserveInput({ origin: "sync", byteLength: 1 }).admitted).toBe(false);
	});

	test("caps a direct port independently while Sync consumes only the worker ceiling", () => {
		const budget = new TerminalInputWorkBudget();
		const directReservations: TerminalInputWorkReservation[] = [];
		for (let index = 0; index < TERMINAL_DIRECT_INPUT_WORK_MAX_REQUESTS; index += 1) {
			const admission = budget.reserveInput({
				origin: "direct",
				portId: "direct-port-a",
				byteLength: 1,
			});
			if (!admission.admitted) throw new Error("direct input reservation unexpectedly refused");
			directReservations.push(admission.reservation);
		}
		expect(budget.reserveInput({
			origin: "direct",
			portId: "direct-port-a",
			byteLength: 1,
		}).admitted).toBe(false);
		expect(budget.reserveInput({ origin: "sync", byteLength: 1 }).admitted).toBe(true);
		for (const reservation of directReservations) reservation.release();
		expect(budget.reserveInput({
			origin: "direct",
			portId: "direct-port-a",
			byteLength: 1,
		}).admitted).toBe(true);

		const directByteBudget = new TerminalInputWorkBudget();
		const directFull = directByteBudget.reserveInput({
			origin: "direct",
			portId: "direct-port-bytes",
			byteLength: TERMINAL_DIRECT_INPUT_WORK_MAX_BYTES,
		});
		if (!directFull.admitted) throw new Error("full direct byte reservation unexpectedly refused");
		expect(directByteBudget.reserveInput({
			origin: "direct",
			portId: "direct-port-bytes",
			byteLength: 1,
		}).admitted).toBe(false);
	});

	test("keeps route-claim slots separate and bounded by port, worker, and actor session", () => {
		const perPortBudget = new TerminalInputWorkBudget();
		const perPortReservations: TerminalInputWorkReservation[] = [];
		for (let index = 0; index < 4; index += 1) {
			const admission = perPortBudget.reserveRouteClaim("direct-port-a", `actor-session-${index}`);
			if (!admission.admitted) throw new Error("direct claim reservation unexpectedly refused");
			perPortReservations.push(admission.reservation);
		}
		expect(perPortBudget.reserveRouteClaim("direct-port-a", "actor-session-4")).toEqual({
			admitted: false,
			reason: "route_claim_busy",
		});
		expect(perPortBudget.reserveRouteClaim("direct-port-b", "actor-session-0")).toEqual({
			admitted: false,
			reason: "route_claim_busy",
		});
		for (const reservation of perPortReservations) reservation.release();

		const workerBudget = new TerminalInputWorkBudget();
		const workerReservations: TerminalInputWorkReservation[] = [];
		for (let index = 0; index < TERMINAL_ROUTE_CLAIM_WORK_MAX_REQUESTS; index += 1) {
			const admission = workerBudget.reserveRouteClaim(
				`direct-port-${Math.floor(index / 4)}`,
				`actor-session-${index}`,
			);
			if (!admission.admitted) throw new Error("worker claim reservation unexpectedly refused");
			workerReservations.push(admission.reservation);
		}
		expect(workerBudget.reserveRouteClaim("direct-port-extra", "actor-session-extra")).toEqual({
			admitted: false,
			reason: "route_claim_busy",
		});
		for (const reservation of workerReservations) reservation.release();
	});
});
