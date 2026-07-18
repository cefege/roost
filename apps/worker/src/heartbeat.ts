// 30-second heartbeat loop. Samples host metrics (CPU / memory / disk / network
// via macOS subprocess tools) and sends them to coord via Connect
// workersHeartbeat, along with git sha, the keeper-stale stamp, and the live
// tailnet reachable_addr. Callers: main.ts (started after runInstall completes).

import type { CoordClient } from "./coord-client.ts";
import { log, signal } from "@roost/shared";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import { KEEPER_BUILD_STAMP } from "./keeper/keeper-stamp.ts";
import { resolveTailnetDnsName } from "./install.ts";
import { execSync } from "node:child_process";

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

// macOS page size: 16384 on Apple Silicon, 4096 on Intel. The previous
// hard-coded 16384 over-reported memory by 4x on Intel Macs. Read once
// at module load; pagesize doesn't change at runtime.
function _readPageSize(): number {
	try {
		const out = execSync("/usr/sbin/sysctl -n hw.pagesize", {
			encoding: "utf8",
			timeout: 1000,
		});
		const n = Number(out.trim());
		return Number.isFinite(n) && n > 0 ? n : 16384;
	} catch {
		return 16384;
	}
}
const PAGE_SIZE = _readPageSize();

// Bandwidth needs two samples to compute bytes-per-second. Hold the
// previous reading so each heartbeat reports the rate over the last
// interval. First sample yields 0 bps (no baseline yet).
let _prevNet: { rxBytes: number; txBytes: number; sampledAtMs: number } | null =
	null;

// Detect the primary network interface — the one routing to the default
// gateway. Falls back to en0 if route lookup fails.
function _detectPrimaryIface(): string {
	try {
		const out = execSync("/sbin/route -n get default", {
			encoding: "utf8",
			timeout: 1000,
		});
		const m = out.match(/interface:\s+(\S+)/);
		if (m?.[1]) return m[1];
	} catch {
		/* fall through */
	}
	return "en0";
}
const PRIMARY_IFACE = _detectPrimaryIface();

// CPU% via `top -l 1 -n 0`. Prints one snapshot then exits; the CPU
// usage line aggregates user+sys across all cores. We report
// 100 - idle so a single-core saturated process on an 8-core machine
// reports ~12.5%, matching what Activity Monitor's "CPU Usage" gauge
// shows. `-n 0` skips per-process listing (faster + smaller stdout).
function _sampleCpuPct(): number {
	try {
		const out = execSync("/usr/bin/top -l 1 -n 0", {
			encoding: "utf8",
			timeout: 2000,
		});
		const m = out.match(
			/CPU usage:\s+[\d.]+%\s+user,\s+[\d.]+%\s+sys,\s+([\d.]+)%\s+idle/,
		);
		if (m?.[1]) {
			const idle = Number(m[1]);
			if (Number.isFinite(idle)) return Math.max(0, Math.min(100, 100 - idle));
		}
	} catch {
		/* fall through */
	}
	return 0;
}

// Per-interface RX/TX byte counters via `netstat -ibn`. The `-I <iface>`
// form has been observed to truncate to 32-bit counters on some macOS
// builds; the global `-ibn` shows 64-bit values for all interfaces and
// we filter by iface name.
function _sampleNetBytes(
	iface: string,
): { rxBytes: number; txBytes: number } | null {
	try {
		const out = execSync("/usr/sbin/netstat -ibn", {
			encoding: "utf8",
			timeout: 1000,
		});
		for (const line of out.split("\n")) {
			const parts = line.trim().split(/\s+/);
			// Header columns vary slightly across macOS versions but the iface
			// name is always parts[0]; we want the first non-Link row for the
			// requested iface (Link rows have <Link#N> in column 3).
			if (parts[0] !== iface) continue;
			if (parts[2]?.startsWith("<Link")) continue;
			// Columns: Name Mtu Network Address Ipkts Ierrs Ibytes Opkts Oerrs Obytes …
			const ibytesIdx = 6;
			const obytesIdx = 9;
			const rx = Number(parts[ibytesIdx]);
			const tx = Number(parts[obytesIdx]);
			if (Number.isFinite(rx) && Number.isFinite(tx))
				return { rxBytes: rx, txBytes: tx };
		}
	} catch {
		/* fall through */
	}
	return null;
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

function collectHostMetrics(): HostMetrics {
	// Cached resampler: return the prior snapshot if it's < 60s old.
	// Skips the subprocess fan-out + leaves _prevNet untouched so the
	// next real sample's bandwidth delta still spans a full 60s window.
	if (
		_cachedHostMetrics &&
		Date.now() - _cachedHostMetrics.sampled_at_ms < HOST_METRICS_INTERVAL_MS
	) {
		return _cachedHostMetrics;
	}
	const sampled_at_ms = Date.now();
	const cpu_pct = _sampleCpuPct();
	let mem_used_bytes = 0;
	let mem_total_bytes = 0;
	let disk_used_bytes = 0;
	let disk_total_bytes = 0;
	let net_rx_bps = 0;
	let net_tx_bps = 0;

	try {
		const vmstat = execSync("vm_stat", { encoding: "utf8", timeout: 1000 });
		const active = Number(
			(vmstat.match(/Pages active:\s+(\d+)/) ?? [])[1] ?? 0,
		);
		const wired = Number(
			(vmstat.match(/Pages wired down:\s+(\d+)/) ?? [])[1] ?? 0,
		);
		const compressed = Number(
			(vmstat.match(/Pages occupied by compressor:\s+(\d+)/) ?? [])[1] ?? 0,
		);
		// Absolute path: LaunchAgent shell PATH doesn't include /usr/sbin, so
		// bare `sysctl` floods stderr with `/bin/sh: sysctl: command not found`
		// every 30s. Same reason vm_stat works — it lives in /usr/bin which
		// is on the default PATH.
		const total = Number(
			(
				execSync("/usr/sbin/sysctl -n hw.memsize", {
					encoding: "utf8",
					timeout: 1000,
				}) ?? "0"
			).trim(),
		);
		mem_total_bytes = total;
		// Apple's "Memory Used" = wired + compressed + active (app memory).
		// Activity Monitor adds purgeable separately; we approximate.
		mem_used_bytes = (active + wired + compressed) * PAGE_SIZE;

		const dfOut = execSync("df -k /", { encoding: "utf8", timeout: 1000 });
		const dfLine = dfOut.split("\n")[1] ?? "";
		const dfParts = dfLine.trim().split(/\s+/);
		disk_total_bytes = Number(dfParts[1] ?? 0) * 1024;
		disk_used_bytes = Number(dfParts[2] ?? 0) * 1024;
	} catch {
		// Non-fatal — heartbeat continues with last known values.
	}

	const net = _sampleNetBytes(PRIMARY_IFACE);
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
	return process.env.GIT_SHA || undefined;
}

/** Start the 30s heartbeat loop. Resolves after the first successful beat. */
export async function startHeartbeat(opts: {
	client: CoordClient;
}): Promise<void> {
	const { client } = opts;
	const beat = async () => {
		try {
			const host_metrics = collectHostMetrics();
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
			await client.workersHeartbeat({
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
