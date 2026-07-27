// Linux host sampler: /proc + `df -k /` + `ip route`. Never throws —
// every failure path yields 0 so a heartbeat is still sent. Counterpart
// of host-sample-darwin.ts; heartbeat.ts picks one at module load.

import { execSync } from "node:child_process";
import { readFileSync } from "node:fs";
import type { HostSample } from "./host-sample-types.ts";

// CPU% needs two /proc/stat readings: the file holds cumulative jiffies
// since boot, so a single read says nothing about current load. The
// first sample after start therefore reports 0.
let _prevCpu: { idle: number; total: number } | null = null;
function _sampleCpuPct(): number {
	try {
		const line = readFileSync("/proc/stat", "utf8").split("\n")[0] ?? "";
		if (!line.startsWith("cpu ")) return 0;
		const fields = line.trim().split(/\s+/).slice(1).map(Number);
		if (fields.some((n) => !Number.isFinite(n))) return 0;
		// user nice system idle iowait irq softirq steal …
		const idle = (fields[3] ?? 0) + (fields[4] ?? 0);
		const total = fields.reduce((a, b) => a + b, 0);
		const prev = _prevCpu;
		_prevCpu = { idle, total };
		if (!prev) return 0;
		const totalDelta = total - prev.total;
		const idleDelta = idle - prev.idle;
		if (totalDelta <= 0) return 0;
		return Math.max(0, Math.min(100, Math.round(100 * (1 - idleDelta / totalDelta))));
	} catch {
		return 0;
	}
}

// Interface routing to the default gateway; `ip` is in /usr/sbin, which
// the systemd unit's PATH includes. eth0 is the fallback.
let _primaryIface: string | null = null;
function primaryIface(): string {
	if (_primaryIface !== null) return _primaryIface;
	_primaryIface = "eth0";
	try {
		const out = execSync("ip route show default", {
			encoding: "utf8",
			timeout: 1000,
		});
		const m = out.match(/\bdev\s+(\S+)/);
		if (m?.[1]) _primaryIface = m[1];
	} catch {
		/* keep the eth0 fallback */
	}
	return _primaryIface;
}

function _sampleNetBytes(
	iface: string,
): { rxBytes: number; txBytes: number } | null {
	try {
		for (const line of readFileSync("/proc/net/dev", "utf8").split("\n")) {
			// "  eth0: 1234 5 0 …" — the counter can butt against the colon
			// when it grows wide, so split on the colon, not on whitespace.
			const m = line.match(/^\s*([^:\s]+):\s*(.*)$/);
			if (!m || m[1] !== iface) continue;
			const parts = (m[2] ?? "").trim().split(/\s+/).map(Number);
			// rx: bytes packets errs drop fifo frame compressed multicast
			// tx: bytes …
			const rx = parts[0] ?? NaN;
			const tx = parts[8] ?? NaN;
			if (Number.isFinite(rx) && Number.isFinite(tx))
				return { rxBytes: rx, txBytes: tx };
		}
	} catch {
		/* fall through */
	}
	return null;
}

export function sampleHost(): HostSample {
	const cpu_pct = _sampleCpuPct();
	let mem_used_bytes = 0;
	let mem_total_bytes = 0;
	let disk_used_bytes = 0;
	let disk_total_bytes = 0;

	try {
		const meminfo = readFileSync("/proc/meminfo", "utf8");
		const totalKb = Number((meminfo.match(/^MemTotal:\s+(\d+)/m) ?? [])[1] ?? 0);
		const availKb = Number(
			(meminfo.match(/^MemAvailable:\s+(\d+)/m) ?? [])[1] ?? 0,
		);
		mem_total_bytes = totalKb * 1024;
		// "Used" the way `free` reports it: everything the kernel can't hand
		// to a new allocation without reclaim.
		mem_used_bytes = Math.max(0, totalKb - availKb) * 1024;
	} catch {
		/* non-fatal */
	}

	try {
		const dfOut = execSync("df -k /", { encoding: "utf8", timeout: 1000 });
		const dfParts = (dfOut.split("\n")[1] ?? "").trim().split(/\s+/);
		disk_total_bytes = Number(dfParts[1] ?? 0) * 1024;
		disk_used_bytes = Number(dfParts[2] ?? 0) * 1024;
	} catch {
		/* non-fatal */
	}

	return {
		cpu_pct,
		mem_used_bytes,
		mem_total_bytes,
		disk_used_bytes,
		disk_total_bytes,
		net: _sampleNetBytes(primaryIface()),
	};
}
