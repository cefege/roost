// 30-second heartbeat loop. Samples host metrics (CPU / memory / disk / network
// via the platform sampler in host-sample-{darwin,linux}.ts) and sends them to
// coord via Connect workersHeartbeat, along with git sha, the keeper-stale
// stamp, and the live tailnet reachable_addr.
// Callers: main.ts (started after runInstall completes).

import type { CoordClient } from "./coord-client.ts";
import { assertNeverPlatform, supportedHostPlatform } from "@roost/shared/platform";
import { log, signal } from "@roost/shared";
import { ROOST_BUILD_SHA } from "@roost/shared/build-identity";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import { KEEPER_BUILD_STAMP } from "./keeper/keeper-stamp.ts";
import { resolveTailnetDnsName } from "./install.ts";
import { sampleHost as sampleDarwin } from "./host-sample-darwin.ts";
import { sampleHost as sampleLinux, sampleCgroupPressure } from "./host-sample-linux.ts";
import type { HostSample } from "./host-sample-types.ts";
import { sampleHost as sampleWindows } from "./host-sample-win32.ts";

const HOST_PLATFORM = supportedHostPlatform();
let sampleHost: () => HostSample | Promise<HostSample>;
let cgroupPressure: typeof sampleCgroupPressure;
switch (HOST_PLATFORM) {
	case "darwin":
		sampleHost = sampleDarwin;
		cgroupPressure = () => null;
		break;
	case "linux":
		sampleHost = sampleLinux;
		cgroupPressure = sampleCgroupPressure;
		break;
	case "win32":
		sampleHost = sampleWindows;
		cgroupPressure = () => null;
		break;
	default:
		assertNeverPlatform(HOST_PLATFORM);
}

const HEARTBEAT_INTERVAL_MS = 30_000;
// Consecutive heartbeat failures. Reset to 0 on a successful beat; when
// it crosses the stall threshold the worker is effectively invisible to
// the fleet, so we surface it once (cooldown-gated) — the per-tick
// log.warn still fires every miss.
const HEARTBEAT_STALL_AFTER = 3;
let _consecutiveMisses = 0;
// reachable_addr = the LIVE tailnet MagicDNS name, sent every beat so a machine
// rename self-heals in coord within 30s (not only at boot via install.ts). The
// worker label is the Tailscale HostName (e.g. worker-host) which does NOT
// resolve; only Self.DNSName (coord-host) does — so the SPA MUST get this from
// here, never synthesize vnc:// from the label. Re-resolving spawns
// `tailscale status`; cache it 5min so the 30s beat doesn't fork a subprocess
// each tick. env ROOST_REACHABLE_ADDR is the fallback when tailscale isn't up.
const REACHABLE_ADDR_TTL_MS = 5 * 60_000;
let _reachableAddr: string | null = null;
let _reachableAddrAt = 0;
function currentReachableAddr(): string | undefined {
	const now = Date.now();
	if (
		_reachableAddr !== null &&
		now - _reachableAddrAt < REACHABLE_ADDR_TTL_MS
	) {
		return _reachableAddr || undefined;
	}
	const resolved =
		resolveTailnetDnsName() || process.env.ROOST_REACHABLE_ADDR || "";
	// Only refresh the cache timestamp on a real resolve; a transient empty
	// (tailscale GUI shim not ready) shouldn't pin an empty value for 5min.
	if (resolved) {
		_reachableAddr = resolved;
		_reachableAddrAt = now;
	}
	return resolved || undefined;
}
// Host metrics (CPU / memory / disk / network) are expensive to sample
// (top -l1 + vm_stat + netstat + df = ~10–30ms of subprocess time each)
// and Activity-Monitor-style instant readings aren't useful at the UI
// cadence. Sample once per minute; in-between heartbeats reuse the
// cached snapshot so coord still receives host_metrics every 30s but
// the underlying values only refresh every 60s. Bandwidth bps is the
// 60s rolling average between the two real samples.
const HOST_METRICS_INTERVAL_MS = 60_000;

// Bandwidth needs two samples to compute bytes-per-second. Hold the
// previous reading so each heartbeat reports the rate over the last
// interval. First sample yields 0 bps (no baseline yet).
let _prevNet: { rxBytes: number; txBytes: number; sampledAtMs: number } | null =
	null;
// Cgroup-v2 throttle trail. mem_used/mem_total above are host-wide, so a worker
// throttled by its own MemoryHigh publishes a perfectly healthy-looking sample
// while its event loop stalls in D-state. Log-only: no HostSample field, no
// proto change. Counts consecutive over-high samples so a long stall leaves
// periodic breadcrumbs instead of one line or a flood.
let _cgroupOverStreak = 0;
let _prevCgroupHighEvents: number | null = null;
const CGROUP_RELOG_EVERY = 10; // ~10 min at the 60s host-metrics cadence

function logCgroupPressure(): void {
	const p = cgroupPressure();
	if (!p) return;
	const delta =
		_prevCgroupHighEvents === null
			? 0
			: Math.max(0, p.highEvents - _prevCgroupHighEvents);
	_prevCgroupHighEvents = p.highEvents;
	if (p.currentBytes > p.highBytes) {
		const first = _cgroupOverStreak === 0;
		_cgroupOverStreak += 1;
		if (first || _cgroupOverStreak % CGROUP_RELOG_EVERY === 0) {
			log.warn("heartbeat", "cgroup_memory_high_exceeded", {
				current_bytes: p.currentBytes,
				high_bytes: p.highBytes,
				high_events_delta: delta,
			});
		}
	} else if (_cgroupOverStreak > 0) {
		log.info("heartbeat", "cgroup_memory_high_cleared", {
			current_bytes: p.currentBytes,
			high_bytes: p.highBytes,
		});
		_cgroupOverStreak = 0;
	}
}

type HostMetrics = {
	cpu_pct: number;
	mem_used_bytes: number;
	mem_total_bytes: number;
	disk_used_bytes: number;
	disk_total_bytes: number;
	net_rx_bps: number;
	net_tx_bps: number;
	sampled_at_ms: number;
};
let _cachedHostMetrics: HostMetrics | null = null;

async function collectHostMetrics(): Promise<HostMetrics> {
	// Cached resampler: return the prior snapshot if it's < 60s old.
	// Skips the subprocess fan-out + leaves _prevNet untouched so the
	// next real sample's bandwidth delta still spans a full 60s window.
	if (
		_cachedHostMetrics &&
		Date.now() - _cachedHostMetrics.sampled_at_ms < HOST_METRICS_INTERVAL_MS
	) {
		return _cachedHostMetrics;
	}
	// After the cache gate: one probe per real 60s sample, which is what
	// CGROUP_RELOG_EVERY is calibrated against.
	logCgroupPressure();
	const sampled_at_ms = Date.now();
	const {
		cpu_pct,
		mem_used_bytes,
		mem_total_bytes,
		disk_used_bytes,
		disk_total_bytes,
		net,
	} = await sampleHost();
	let net_rx_bps = 0;
	let net_tx_bps = 0;

	if (net && _prevNet) {
		const dtSec = (sampled_at_ms - _prevNet.sampledAtMs) / 1000;
		if (dtSec > 0) {
			// Counters can wrap or reset (interface down/up). Negative deltas
			// → drop the sample; bandwidth shows as 0 for this tick rather
			// than negative or a giant wrap-around number.
			const rxDelta = net.rxBytes - _prevNet.rxBytes;
			const txDelta = net.txBytes - _prevNet.txBytes;
			if (rxDelta >= 0) net_rx_bps = Math.round(rxDelta / dtSec);
			if (txDelta >= 0) net_tx_bps = Math.round(txDelta / dtSec);
		}
	}
	if (net) _prevNet = { ...net, sampledAtMs: sampled_at_ms };

	_cachedHostMetrics = {
		cpu_pct,
		mem_used_bytes,
		mem_total_bytes,
		disk_used_bytes,
		disk_total_bytes,
		net_rx_bps,
		net_tx_bps,
		sampled_at_ms,
	};
	return _cachedHostMetrics;
}

function getGitSha(): string | undefined {
	return ROOST_BUILD_SHA === "dev" ? undefined : ROOST_BUILD_SHA;
}

/** Start the 30s heartbeat loop. Resolves after the first successful beat. */
export async function startHeartbeat(opts: {
	client: () => CoordClient;
}): Promise<void> {
	const { client } = opts;
	const beat = async () => {
		try {
			const host_metrics = await collectHostMetrics();
			const git_sha = getGitSha();
			// keeper_stale: the running keeper's stamp when it differs from ours
			// (keeper running stale code), "" when current, undefined until known.
			const runningKeeperStamp = getMultiplexedPool().getRunningKeeperStamp();
			const keeper_stale =
				runningKeeperStamp === null
					? undefined
					: runningKeeperStamp !== KEEPER_BUILD_STAMP
						? runningKeeperStamp
						: "";
			const reachable_addr = currentReachableAddr();
			await client().workersHeartbeat({
				hostMetrics: host_metrics
					? {
							cpuPct: host_metrics.cpu_pct,
							memUsedBytes: BigInt(host_metrics.mem_used_bytes),
							memTotalBytes: BigInt(host_metrics.mem_total_bytes),
							diskUsedBytes: BigInt(host_metrics.disk_used_bytes),
							diskTotalBytes: BigInt(host_metrics.disk_total_bytes),
							netRxBps: BigInt(host_metrics.net_rx_bps),
							netTxBps: BigInt(host_metrics.net_tx_bps),
							sampledAtMs: BigInt(host_metrics.sampled_at_ms),
						}
					: undefined,
				...(git_sha ? { gitSha: git_sha } : {}),
				...(keeper_stale !== undefined ? { keeperStale: keeper_stale } : {}),
				...(reachable_addr ? { reachableAddr: reachable_addr } : {}),
			});
			log.debug("heartbeat", "beat sent", { reachable_addr });
			_consecutiveMisses = 0;
		} catch (e) {
			log.warn("heartbeat", "beat failed", { error: String(e) });
			_consecutiveMisses += 1;
			if (_consecutiveMisses >= HEARTBEAT_STALL_AFTER) {
				signal("heartbeat.stalled", { misses: _consecutiveMisses, cooldownKey: "heartbeat" });
			}
		}
	};

	// First beat — awaited so caller knows coord sees us before continuing.
	await beat();

	// Recurring loop.
	setInterval(beat, HEARTBEAT_INTERVAL_MS);
}
