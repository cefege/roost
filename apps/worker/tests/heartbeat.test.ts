import { afterEach, describe, expect, setSystemTime, test, vi } from "bun:test";
import { Code, ConnectError } from "@connectrpc/connect";
import { setSignalSink } from "@roost/shared/diag";
import type { HostMetrics } from "@roost/shared/wire";
import type { CoordClient } from "../src/coord-client.ts";
import {
	HEARTBEAT_INTERVAL_MS,
	HEARTBEAT_RPC_TIMEOUT_MS,
	startHeartbeat,
	type HeartbeatSources,
} from "../src/heartbeat.ts";

const METRICS: HostMetrics = {
	cpu_pct: 12.5,
	mem_used_bytes: 100,
	mem_total_bytes: 200,
	disk_used_bytes: 300,
	disk_total_bytes: 400,
	net_rx_bps: 500,
	net_tx_bps: 600,
	sampled_at_ms: 700,
};

function sources(
	collectHostMetrics: HeartbeatSources["collectHostMetrics"] = async () => METRICS,
): HeartbeatSources {
	return {
		collectHostMetrics,
		getGitSha: () => "test-sha",
		getRunningKeeperStamp: () => null,
		getReachableAddr: () => "worker.test",
	};
}

type HeartbeatRpc = (
	request: unknown,
	options: { timeoutMs: number },
) => Promise<unknown>;

function clientWith(
	workersHeartbeat: HeartbeatRpc,
): CoordClient {
	return { workersHeartbeat: vi.fn(workersHeartbeat) } as unknown as CoordClient;
}

async function settle(): Promise<void> {
	for (let i = 0; i < 8; i++) await Promise.resolve();
}

afterEach(() => {
	setSignalSink(null);
	vi.useRealTimers();
});

describe("worker heartbeat supervision", () => {
	test("awaits the first attempt and applies the exact RPC deadline", async () => {
		vi.useFakeTimers();
		setSystemTime(new Date(1_000_000));
		const gate = Promise.withResolvers<unknown>();
		const rpc = vi.fn((_request: unknown, _options: unknown) => gate.promise);
		const starting = startHeartbeat({ client: () => clientWith(rpc), sources: sources() });
		let returned = false;
		void starting.then(() => { returned = true; });
		await settle();
		expect(returned).toBe(false);
		expect(rpc).toHaveBeenCalledTimes(1);
		expect(rpc.mock.calls[0]?.[1]).toEqual({ timeoutMs: HEARTBEAT_RPC_TIMEOUT_MS });
		gate.resolve({});
		const dispose = await starting;
		expect(returned).toBe(true);
		dispose();
		vi.advanceTimersByTime(HEARTBEAT_INTERVAL_MS * 4);
		expect(rpc).toHaveBeenCalledTimes(1);
	});

	test("never overlaps calls and schedules the next attempt from settlement", async () => {
		vi.useFakeTimers();
		setSystemTime(new Date(2_000_000));
		const second = Promise.withResolvers<unknown>();
		let attempts = 0;
		const rpc = vi.fn(() => {
			attempts += 1;
			return attempts === 2 ? second.promise : Promise.resolve({});
		});
		const dispose = await startHeartbeat({ client: () => clientWith(rpc), sources: sources() });
		vi.advanceTimersByTime(HEARTBEAT_INTERVAL_MS);
		await settle();
		expect(rpc).toHaveBeenCalledTimes(2);
		vi.advanceTimersByTime(HEARTBEAT_INTERVAL_MS * 10);
		await settle();
		expect(rpc).toHaveBeenCalledTimes(2);
		second.resolve({});
		await settle();
		vi.advanceTimersByTime(HEARTBEAT_INTERVAL_MS - 1);
		expect(rpc).toHaveBeenCalledTimes(2);
		vi.advanceTimersByTime(1);
		await settle();
		expect(rpc).toHaveBeenCalledTimes(3);
		dispose();
		vi.advanceTimersByTime(HEARTBEAT_INTERVAL_MS * 4);
		expect(rpc).toHaveBeenCalledTimes(3);
	});

	test("retains the last good metrics when collection is unknown", async () => {
		vi.useFakeTimers();
		setSystemTime(new Date(3_000_000));
		let samples = 0;
		const rpc = vi.fn(async (_request: unknown, _options: { timeoutMs: number }) => ({}));
		const dispose = await startHeartbeat({
			client: () => clientWith(rpc),
			sources: sources(async () => {
				samples += 1;
				if (samples === 2) throw new Error("sample unavailable");
				return METRICS;
			}),
		});
		vi.advanceTimersByTime(HEARTBEAT_INTERVAL_MS);
		await settle();
		expect(rpc).toHaveBeenCalledTimes(2);
		expect(rpc.mock.calls[1]?.[0]).toEqual(rpc.mock.calls[0]?.[0]);
		expect(rpc.mock.calls[1]?.[1]).toEqual({ timeoutMs: HEARTBEAT_RPC_TIMEOUT_MS });
		dispose();
	});

	test("counts one miss per settlement, resets on success, and isolates instances", async () => {
		vi.useFakeTimers();
		setSystemTime(new Date(4_000_000));
		const signals: Array<Record<string, unknown>> = [];
		setSignalSink((record) => signals.push(record));
		const rejected = () => Promise.reject(new ConnectError("deadline", Code.DeadlineExceeded));
		const firstRpc = vi.fn(rejected);
		const secondRpc = vi.fn(rejected);
		const stopFirst = await startHeartbeat({ client: () => clientWith(firstRpc), sources: sources() });
		const stopSecond = await startHeartbeat({ client: () => clientWith(secondRpc), sources: sources() });
		vi.advanceTimersByTime(HEARTBEAT_INTERVAL_MS);
		await settle();
		expect(signals).toEqual([]);
		stopFirst();
		stopSecond();
		let attempt = 0;
		const resetRpc = vi.fn((_request: unknown, _options: { timeoutMs: number }) => {
			attempt += 1;
			return attempt === 3 ? Promise.resolve({}) : rejected();
		});
		const stopReset = await startHeartbeat({ client: () => clientWith(resetRpc), sources: sources() });
		for (let i = 0; i < 5; i++) {
			vi.advanceTimersByTime(HEARTBEAT_INTERVAL_MS);
			await settle();
		}
		expect(signals).toHaveLength(1);
		expect(signals[0]).toMatchObject({ evt: "heartbeat.stalled", misses: 3 });
		for (const call of resetRpc.mock.calls) {
			expect(call[1]).toEqual({ timeoutMs: HEARTBEAT_RPC_TIMEOUT_MS });
		}
		stopReset();
	});
});
