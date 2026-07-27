// macOS host sampler: top / vm_stat / sysctl / df / netstat.
// Moved verbatim out of heartbeat.ts when the Linux sampler landed; the
// only change is that pagesize and the primary interface are now lazily
// memoized instead of read at module load, so importing this file on a
// Linux worker costs nothing.

import { execSync } from "node:child_process";
import type { HostSample } from "./host-sample-types.ts";

// macOS page size: 16384 on Apple Silicon, 4096 on Intel. The previous
// hard-coded 16384 over-reported memory by 4x on Intel Macs. Read once;
// pagesize doesn't change at runtime.
let _pageSize: number | null = null;
function pageSize(): number {
	if (_pageSize !== null) return _pageSize;
	try {
		const out = execSync("/usr/sbin/sysctl -n hw.pagesize", {
			encoding: "utf8",
			timeout: 1000,
		});
		const n = Number(out.trim());
		_pageSize = Number.isFinite(n) && n > 0 ? n : 16384;
	} catch {
		_pageSize = 16384;
	}
	return _pageSize;
}

// Detect the primary network interface — the one routing to the default
// gateway. Falls back to en0 if route lookup fails.
let _primaryIface: string | null = null;
function primaryIface(): string {
	if (_primaryIface !== null) return _primaryIface;
	_primaryIface = "en0";
	try {
		const out = execSync("/sbin/route -n get default", {
			encoding: "utf8",
			timeout: 1000,
		});
		const m = out.match(/interface:\s+(\S+)/);
		if (m?.[1]) _primaryIface = m[1];
	} catch {
		/* keep the en0 fallback */
	}
	return _primaryIface;
}

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

export function sampleHost(): HostSample {
	const cpu_pct = _sampleCpuPct();
	let mem_used_bytes = 0;
	let mem_total_bytes = 0;
	let disk_used_bytes = 0;
	let disk_total_bytes = 0;

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
		mem_used_bytes = (active + wired + compressed) * pageSize();

		const dfOut = execSync("df -k /", { encoding: "utf8", timeout: 1000 });
		const dfLine = dfOut.split("\n")[1] ?? "";
		const dfParts = dfLine.trim().split(/\s+/);
		disk_total_bytes = Number(dfParts[1] ?? 0) * 1024;
		disk_used_bytes = Number(dfParts[2] ?? 0) * 1024;
	} catch {
		// Non-fatal — heartbeat continues with last known values.
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
