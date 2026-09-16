// Host memory ceiling detection: the tightest cgroup-v2 limit this process
// runs under, falling back to total host memory. Lives here because two apps
// need the same number: the worker (host-sample-linux.ts cgroup pressure,
// terminal-core-capacity.ts core admission) and the coordinator (its terminal
// cell-replica memory budget). Depends only on node:fs and node:os.

import { readFileSync } from "node:fs";
import { totalmem } from "node:os";

export type LinuxMemoryFileReader = (path: string) => string;

export function readLinuxMemoryFile(path: string): string {
	return readFileSync(path, "utf8");
}

export function cgroupV2Base(readText: LinuxMemoryFileReader): string | null {
	const relativePath = (
		readText("/proc/self/cgroup").match(/^0::(.*)$/m) ?? []
	)[1];
	if (relativePath === undefined || !relativePath.startsWith("/")) return null;
	return relativePath === "/"
		? "/sys/fs/cgroup"
		: `/sys/fs/cgroup${relativePath}`;
}

function finiteCgroupMemoryLimit(value: string): number | null {
	const normalized = value.trim();
	if (!/^\d+$/.test(normalized)) return null;
	const bytes = Number(normalized);
	return Number.isSafeInteger(bytes) && bytes >= 0 ? bytes : null;
}

/** The worker's memory ceiling is the tightest finite cgroup-v2 high/max
 * setting; without either cgroup limit, host memory is the only ceiling. */
export function effectiveLinuxMemoryCeilingBytes(
	hostMemoryBytes: number = totalmem(),
	readText: LinuxMemoryFileReader = readLinuxMemoryFile,
): number {
	const hostMemory = Number.isSafeInteger(hostMemoryBytes) && hostMemoryBytes >= 0
		? hostMemoryBytes
		: 0;
	try {
		const base = cgroupV2Base(readText);
		if (base === null) return hostMemory;
		const limits = [
			finiteCgroupMemoryLimit(readText(`${base}/memory.high`)),
			finiteCgroupMemoryLimit(readText(`${base}/memory.max`)),
		].filter((limit): limit is number => limit !== null);
		return limits.length === 0 ? hostMemory : Math.min(...limits);
	} catch {
		return hostMemory;
	}
}

/** Platform-dispatching ceiling: only Linux exposes a cgroup limit, so every
 * other platform is bounded by host memory alone. */
export function effectiveMemoryCeilingBytes(
	platform: NodeJS.Platform = process.platform,
	hostMemoryBytes: number = totalmem(),
	readText: LinuxMemoryFileReader = readLinuxMemoryFile,
): number {
	return platform === "linux"
		? effectiveLinuxMemoryCeilingBytes(hostMemoryBytes, readText)
		: hostMemoryBytes;
}
