// Terminal-core capacity admission tests use deterministic memory and core seams.
// They cover steady cap math, refusal, replacement serialization, and record teardown.
// Keeper and WTerm processes stay stubbed so no host memory or PTY timing affects results.

import { afterEach, describe, expect, test } from "bun:test";
import type { TerminalCore } from "@wterm/core";
import { asWorkerFp } from "@roost/shared/wire";
import { getMultiplexedPool } from "../src/keeper/multiplexed-client.ts";
import { SessionManager } from "../src/session-manager.ts";
import {
	TERMINAL_CORE_ALLOCATION_BYTES,
	TERMINAL_CORE_CAPACITY_ERROR_CODE,
	TERMINAL_CORE_CAPACITY_ERROR_MESSAGE,
	TERMINAL_CORE_CAPACITY_HARD_MAX,
	TerminalCoreCapacity,
	TerminalCoreCapacityError,
} from "../src/terminal-core-capacity.ts";
import { effectiveLinuxMemoryCeilingBytes } from "../src/host-sample-linux.ts";
import { SessionEventTestSink } from "./session-event-test-sink.ts";

const WORKER_FP = asWorkerFp("42".repeat(32));
const TEST_CEILING_BYTES = 1_000 * TERMINAL_CORE_ALLOCATION_BYTES;
const pool = getMultiplexedPool();
const originalSpawn = pool.spawn;
const managers: SessionManager[] = [];

afterEach(() => {
	pool.spawn = originalSpawn;
	for (const manager of managers.splice(0)) manager.dispose();
});

function capacityWithLimit(terminalCoreCap: number): TerminalCoreCapacity {
	return new TerminalCoreCapacity({
		effectiveMemoryCeilingBytes: TEST_CEILING_BYTES,
		bootRssBytes: 0,
		terminalCoreCap,
	});
}

function expectCapacityRefusal(action: () => void): void {
	let thrown: unknown;
	try {
		action();
	} catch (error) {
		thrown = error;
	}
	expect(thrown).toBeInstanceOf(TerminalCoreCapacityError);
	if (!(thrown instanceof TerminalCoreCapacityError)) return;
	expect(thrown.code).toBe(TERMINAL_CORE_CAPACITY_ERROR_CODE);
	expect(thrown.message).toBe(TERMINAL_CORE_CAPACITY_ERROR_MESSAGE);
}

describe("TerminalCoreCapacity", () => {
	test("calculates the steady cap and exposes only the heartbeat snapshot shape", () => {
		const ceilingBytes = 35 * TERMINAL_CORE_ALLOCATION_BYTES;
		const bootRssBytes = 20 * TERMINAL_CORE_ALLOCATION_BYTES;
		const capacity = new TerminalCoreCapacity({
			effectiveMemoryCeilingBytes: ceilingBytes,
			bootRssBytes,
		});

		expect(capacity.snapshot()).toEqual({
			used: 0,
			pending: 0,
			capacity: 3,
			estimatedReservedBytes: 0,
			effectiveMemoryCeilingBytes: ceilingBytes,
			bootRssBytes,
			overcommitCount: 0,
			refusalCount: 0,
		});
		expect(capacityWithLimit(2).snapshot().capacity).toBe(2);
		const highOperatorCap = new TerminalCoreCapacity({
			effectiveMemoryCeilingBytes: 11 * TERMINAL_CORE_ALLOCATION_BYTES,
			bootRssBytes: 0,
			terminalCoreCap: 99,
		});
		expect(highOperatorCap.snapshot().capacity).toBe(6);
		const hardCapped = new TerminalCoreCapacity({
			effectiveMemoryCeilingBytes: TEST_CEILING_BYTES,
			bootRssBytes: 0,
		});
		expect(hardCapped.snapshot().capacity).toBe(TERMINAL_CORE_CAPACITY_HARD_MAX);
	});

	test("reads the lowest finite cgroup limit and falls back to host memory", () => {
		const cgroupRoot = "/sys/fs/cgroup/roost-worker";
		const cgroupReader = (memoryHigh: string, memoryMax: string) => (path: string): string => {
			if (path === "/proc/self/cgroup") return "0::/roost-worker\n";
			if (path === `${cgroupRoot}/memory.high`) return memoryHigh;
			if (path === `${cgroupRoot}/memory.max`) return memoryMax;
			throw new Error(`unexpected cgroup path ${path}`);
		};

		expect(effectiveLinuxMemoryCeilingBytes(
			800,
			cgroupReader("600", "700"),
		)).toBe(600);
		expect(effectiveLinuxMemoryCeilingBytes(
			800,
			cgroupReader("max", "max"),
		)).toBe(800);
		const rootCgroupReader = (path: string): string => {
			if (path === "/proc/self/cgroup") return "0::/\n";
			if (path === "/sys/fs/cgroup/memory.high") return "max";
			if (path === "/sys/fs/cgroup/memory.max") return "512";
			throw new Error(`unexpected root cgroup path ${path}`);
		};
		expect(effectiveLinuxMemoryCeilingBytes(800, rootCgroupReader)).toBe(512);
	});

	test("refuses fresh and adoption allocation at zero or full capacity", () => {
		const zeroCapacity = capacityWithLimit(0);
		expectCapacityRefusal(() => zeroCapacity.reserveFresh());
		expectCapacityRefusal(() => zeroCapacity.reserveAdoption());
		expect(zeroCapacity.snapshot().refusalCount).toBe(2);

		const fullCapacity = capacityWithLimit(1);
		const lease = fullCapacity.reserveFresh();
		lease.activate();
		expectCapacityRefusal(() => fullCapacity.reserveFresh());
		expectCapacityRefusal(() => fullCapacity.reserveAdoption());
		expect(fullCapacity.snapshot()).toMatchObject({
			used: 1,
			pending: 0,
			capacity: 1,
			refusalCount: 2,
		});
		lease.release();
	});

	test("releases a pending lease when terminal-core construction fails", async () => {
		const capacity = capacityWithLimit(1);
		const manager = new SessionManager({
			workerFp: WORKER_FP,
			sink: new SessionEventTestSink(),
			terminalCoreCapacity: capacity,
			createTerminalCore: async () => {
				throw new Error("wterm construction failed");
			},
		});
		managers.push(manager);
		const lease = capacity.reserveFresh();

		await expect(manager.createTerminalCoreForLease(lease, 80, 24))
			.rejects.toThrow("wterm construction failed");
		expect(capacity.snapshot()).toMatchObject({ used: 0, pending: 0 });
	});

	test("serializes replacement headroom until old-record teardown completes", () => {
		const capacity = capacityWithLimit(1);
		const oldLease = capacity.reserveFresh();
		oldLease.activate();
		const replacementLease = capacity.reserveReplacement();
		expect(capacity.snapshot()).toMatchObject({
			used: 1,
			pending: 1,
			overcommitCount: 1,
		});
		expectCapacityRefusal(() => capacity.reserveReplacement());

		replacementLease.activate();
		oldLease.release();
		expect(capacity.snapshot()).toMatchObject({
			used: 1,
			pending: 0,
			overcommitCount: 0,
		});
		expectCapacityRefusal(() => capacity.reserveReplacement());
		capacity.completeReplacement(replacementLease);

		const nextReplacement = capacity.reserveReplacement();
		nextReplacement.release();
		replacementLease.release();
	});

	test("releases the production lease when channel state tears down", async () => {
		const capacity = capacityWithLimit(1);
		const manager = new SessionManager({
			workerFp: WORKER_FP,
			sink: new SessionEventTestSink(),
			terminalCoreCapacity: capacity,
			createTerminalCore: async (cols, rows) => ({
				getCols: () => cols,
				getRows: () => rows,
			}) as unknown as TerminalCore,
		});
		manager._startGitBranch = () => undefined;
		manager._startPorts = () => undefined;
		managers.push(manager);
		pool.spawn = async () => 4242;

		const record = await manager.spawnShell(process.cwd(), 80, 24);
		expect(capacity.snapshot()).toMatchObject({ used: 1, pending: 0 });
		manager.releaseSessionEvent(record.closeReservation);
		manager._dropChannelState(record.channelId);
		expect(capacity.snapshot()).toMatchObject({ used: 0, pending: 0 });
	});
});
