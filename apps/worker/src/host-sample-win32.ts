// Windows host sampler. Node exposes truthful CPU-time and memory counters;
// statfs covers the active system volume. The project-owned Win32 helper
// supplies IP Helper byte counters and a native fallback for every field.

import { statfsSync } from "node:fs";
import { cpus, freemem, totalmem } from "node:os";
import { win32 } from "node:path";
import { windowsHostSample } from "@roost/shared/windows-helper";
import type { HostSample } from "./host-sample-types.ts";

type CpuTimes = { idle: number; total: number };
let previousCpuTimes: CpuTimes | null = null;

function nodeCpuPercentage(fallback: number): number {
  const times = cpus().reduce<CpuTimes>((sum, cpu) => {
    sum.idle += cpu.times.idle;
    sum.total += cpu.times.user + cpu.times.nice + cpu.times.sys + cpu.times.idle + cpu.times.irq;
    return sum;
  }, { idle: 0, total: 0 });
  const previous = previousCpuTimes;
  previousCpuTimes = times;
  if (!previous) return Math.max(0, Math.min(100, fallback));
  const totalDelta = times.total - previous.total;
  const idleDelta = times.idle - previous.idle;
  if (totalDelta <= 0 || idleDelta < 0) return Math.max(0, Math.min(100, fallback));
  return Math.max(0, Math.min(100, Math.round(100 * (1 - idleDelta / totalDelta))));
}

function systemVolumeRoot(): string {
  const configured = process.env.SystemDrive ?? process.env.SYSTEMDRIVE;
  if (configured && /^[A-Za-z]:$/.test(configured)) return `${configured}\\`;
  const cwdRoot = win32.parse(process.cwd()).root;
  if (cwdRoot) return cwdRoot;
  throw new Error("unable to resolve Windows system volume");
}

export async function sampleHost(): Promise<HostSample> {
  const native = await windowsHostSample();
  const nodeTotal = totalmem();
  const nodeFree = freemem();
  const mem_total_bytes = Number.isFinite(nodeTotal) && nodeTotal > 0
    ? nodeTotal
    : native.mem_total_bytes;
  const mem_used_bytes = Number.isFinite(nodeFree) && nodeFree >= 0 && nodeFree <= mem_total_bytes
    ? mem_total_bytes - nodeFree
    : native.mem_used_bytes;

  let disk_total_bytes = native.disk_total_bytes;
  let disk_used_bytes = native.disk_used_bytes;
  try {
    const volume = statfsSync(systemVolumeRoot());
    const total = Number(volume.bsize) * Number(volume.blocks);
    const free = Number(volume.bsize) * Number(volume.bfree);
    if (Number.isFinite(total) && Number.isFinite(free) && total > 0 && free >= 0 && free <= total) {
      disk_total_bytes = total;
      disk_used_bytes = total - free;
    }
  } catch {
    // The native helper already returned a validated volume sample.
  }

  return {
    cpu_pct: nodeCpuPercentage(native.cpu_pct),
    mem_used_bytes,
    mem_total_bytes,
    disk_used_bytes,
    disk_total_bytes,
    net: native.net,
  };
}
