import { afterEach, describe, expect, setSystemTime, test, vi } from "bun:test";
import { Code, ConnectError } from "@connectrpc/connect";
import { setSignalSink } from "@roost/shared/diag";
import type { KeeperRuntimeObservationV1 } from "@roost/shared/keeper-update";
import { keeperRuntimeObservationFromProto } from "@roost/shared/keeper-update-proto";
import type {
	KeeperRuntimeObservationV1 as KeeperRuntimeObservationProto,
	TerminalCoreCapacityReport as TerminalCoreCapacityReportProto,
} from "@roost/shared/proto/wire_pb";
import { terminalCoreCapacityReportFromProto } from "@roost/shared/terminal-core-capacity-proto";
import type { HostMetrics, TerminalCoreCapacityReport } from "@roost/shared/wire";
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
		observeKeeperRuntime: async () => null,
		getReachableAddr: () => "worker.test",
	};
}

interface HeartbeatBeat {
	keeperRuntime?: KeeperRuntimeObservationProto;
	terminalCoreCapacity?: TerminalCoreCapacityReportProto;
}

type HeartbeatRpc = (
	request: HeartbeatBeat,
	options: { timeoutMs: number },
) => Promise<unknown>;

const TERMINAL_CORE_CAPACITY: TerminalCoreCapacityReport = {
	used: 2,
	pending: 1,
	capacity: 2,
	estimated_reserved_bytes: 120 * 1024 * 1024,
	effective_memory_ceiling_bytes: 2 * 1024 * 1024 * 1024,
	boot_rss_bytes: 256 * 1024 * 1024,
	overcommit_count: 1,
	refusal_count: 4,
};

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
		const starting = startHeartbeat({ reconciledAtMs: () => 100, client: () => clientWith(rpc), sources: sources() });
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
		const dispose = await startHeartbeat({ reconciledAtMs: () => 100, client: () => clientWith(rpc), sources: sources() });
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
			reconciledAtMs: () => 100,
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
		const stopFirst = await startHeartbeat({ reconciledAtMs: () => 100, client: () => clientWith(firstRpc), sources: sources() });
		const stopSecond = await startHeartbeat({ reconciledAtMs: () => 100, client: () => clientWith(secondRpc), sources: sources() });
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
		const stopReset = await startHeartbeat({ reconciledAtMs: () => 100, client: () => clientWith(resetRpc), sources: sources() });
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

	describe("terminal core capacity reporting", () => {
		test("ships the current worker-owned capacity snapshot", async () => {
			const readTerminalCoreCapacity = vi.fn(() => TERMINAL_CORE_CAPACITY);
			const rpc = vi.fn(async (
				_request: HeartbeatBeat,
				_options: { timeoutMs: number },
			) => ({}));
			const dispose = await startHeartbeat({
				reconciledAtMs: () => null,
				client: () => clientWith(rpc),
				readTerminalCoreCapacity,
				sources: sources(),
			});

			expect(readTerminalCoreCapacity).toHaveBeenCalledTimes(1);
			expect(terminalCoreCapacityReportFromProto(
				rpc.mock.calls[0]?.[0].terminalCoreCapacity!,
			)).toEqual(TERMINAL_CORE_CAPACITY);
			dispose();
		});
	});
});

const OBSERVATION: KeeperRuntimeObservationV1 = {
	schema_version: 1,
	running_contract: {
		protocol_version: 3,
		supported_features: ["history-records", "spawn-epoch"],
		required_features: ["history-records"],
		implementation_digest: "b".repeat(64),
		bun_abi: "1.3.14",
		platform: "linux",
		arch: "x64",
		build_sha: "c".repeat(40),
	},
	keeper_pid: 4242,
	keeper_epoch: "3f2504e0-4f89-41d3-9a0c-0305e82c3301",
	channel_count: 2,
	binding_digest: "a".repeat(64),
	reconciled_at_ms: 1_700_000_000_000,
};

function keeperRuntimeOf(
	request: HeartbeatBeat | undefined,
): KeeperRuntimeObservationV1 | null {
	if (!request?.keeperRuntime) return null;
	return keeperRuntimeObservationFromProto(request.keeperRuntime);
}

describe("keeper runtime reporting", () => {
	test("withholds the observation until boot reconciliation has succeeded", async () => {
		const observe = vi.fn(async () => OBSERVATION);
		const rpc = vi.fn(async (_request: HeartbeatBeat, _options: { timeoutMs: number }) => ({}));
		const dispose = await startHeartbeat({
			reconciledAtMs: () => null,
			client: () => clientWith(rpc),
			sources: { ...sources(), observeKeeperRuntime: observe },
		});
		expect(observe).not.toHaveBeenCalled();
		expect(keeperRuntimeOf(rpc.mock.calls[0]?.[0])).toBeNull();
		dispose();
	});

	test("ships the proved observation stamped with the reconciliation it belongs to", async () => {
		const observe = vi.fn(async (reconciledAtMs: number) => ({
			...OBSERVATION,
			reconciled_at_ms: reconciledAtMs,
		}));
		const rpc = vi.fn(async (_request: HeartbeatBeat, _options: { timeoutMs: number }) => ({}));
		const dispose = await startHeartbeat({
			reconciledAtMs: () => 1_700_000_000_777,
			client: () => clientWith(rpc),
			sources: { ...sources(), observeKeeperRuntime: observe },
		});
		expect(observe).toHaveBeenCalledWith(1_700_000_000_777);
		expect(keeperRuntimeOf(rpc.mock.calls[0]?.[0])).toEqual({
			...OBSERVATION,
			reconciled_at_ms: 1_700_000_000_777,
		});
		dispose();
	});

	test("drops an observation whose reconciliation was superseded mid-beat", async () => {
		let reconciled: number | null = 1_700_000_000_111;
		const rpc = vi.fn(async (_request: HeartbeatBeat, _options: { timeoutMs: number }) => ({}));
		const dispose = await startHeartbeat({
			reconciledAtMs: () => reconciled,
			client: () => clientWith(rpc),
			sources: {
				...sources(),
				observeKeeperRuntime: async () => {
					reconciled = null;
					return OBSERVATION;
				},
			},
		});
		expect(keeperRuntimeOf(rpc.mock.calls[0]?.[0])).toBeNull();
		dispose();
	});

	test("still beats when the keeper probe fails", async () => {
		const rpc = vi.fn(async (_request: HeartbeatBeat, _options: { timeoutMs: number }) => ({}));
		const dispose = await startHeartbeat({
			reconciledAtMs: () => 1_700_000_000_222,
			client: () => clientWith(rpc),
			sources: {
				...sources(),
				observeKeeperRuntime: async () => { throw new Error("keeper unreachable"); },
			},
		});
		expect(rpc).toHaveBeenCalledTimes(1);
		expect(keeperRuntimeOf(rpc.mock.calls[0]?.[0])).toBeNull();
		dispose();
	});
});
