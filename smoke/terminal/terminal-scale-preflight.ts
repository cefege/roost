// Scale-only capacity and host-resource preflight for terminal qualifications.
// The local fixture stack has one shared memory/cgroup budget, so this module
// never inflates that budget by summing duplicate per-process capacity reports.
// A 500-session run requires one fixture worker to report every required core.

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import type { TerminalTestStack, TerminalTestWorker } from "./stack.ts";

const CAPACITY_REPORT_TIMEOUT_MS = 30_000;
const MiB = 1024n * 1024n;
const RESERVED_BYTES_PER_SESSION = 40n * MiB;
const MINIMUM_OPEN_FILES = 4_096n;
const MINIMUM_TASKS = 2_048n;
const PID_HEADROOM = 64n;

export interface ScaleWorkerCapacity {
  workerFp: string;
  label: string;
  used: number;
  pending: number;
  capacity: number;
  available: number;
  estimatedReservedBytes: bigint;
  effectiveMemoryCeilingBytes: bigint;
  bootRssBytes: bigint;
  overcommitCount: number;
  refusalCount: bigint;
}

export interface ScaleResourcePreflight {
  requiredReservedBytes: bigint;
  openFiles: bigint | "unlimited" | null;
  maxProcesses: bigint | "unlimited" | null;
  cgroupMemoryHigh: bigint | "unlimited" | null;
  cgroupMemoryMax: bigint | "unlimited" | null;
  cgroupPidsCurrent: bigint | null;
  cgroupPidsMax: bigint | "unlimited" | null;
}

export class ScaleCapacityPreflightError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ScaleCapacityPreflightError";
  }
}

export function workerFolder(worker: TerminalTestWorker): string {
  return process.platform === "win32" ? worker.home.replaceAll("\\", "/") : worker.home;
}

function requireUInt32(value: number, field: string): number {
  if (!Number.isSafeInteger(value) || value < 0 || value > 0xffffffff) {
    throw new ScaleCapacityPreflightError(`worker capacity ${field} was not a uint32`);
  }
  return value;
}

function requireUInt64(value: bigint, field: string): bigint {
  if (value < 0n) throw new ScaleCapacityPreflightError(`worker capacity ${field} was negative`);
  return value;
}

export async function waitForWorkerCapacity(
  stack: TerminalTestStack,
  worker: TerminalTestWorker,
  timeoutMs = CAPACITY_REPORT_TIMEOUT_MS,
): Promise<ScaleWorkerCapacity> {
  const deadline = Date.now() + timeoutMs;
  let lastState = "worker absent";
  while (Date.now() < deadline) {
    const { workers, routableFps } = await stack.client.workersList({});
    const candidate = workers.find((item) => item.fp === worker.workerFp);
    const report = candidate?.terminalCoreCapacity;
    if (candidate && routableFps.includes(worker.workerFp) && report) {
      const used = requireUInt32(report.used, "used");
      const pending = requireUInt32(report.pending, "pending");
      const capacity = requireUInt32(report.capacity, "capacity");
      const overcommitCount = requireUInt32(report.overcommitCount, "overcommit_count");
      const available = Math.max(0, capacity - used - pending);
      return {
        workerFp: worker.workerFp,
        label: worker.label,
        used,
        pending,
        capacity,
        available,
        estimatedReservedBytes: requireUInt64(report.estimatedReservedBytes, "estimated_reserved_bytes"),
        effectiveMemoryCeilingBytes: requireUInt64(
          report.effectiveMemoryCeilingBytes,
          "effective_memory_ceiling_bytes",
        ),
        bootRssBytes: requireUInt64(report.bootRssBytes, "boot_rss_bytes"),
        overcommitCount,
        refusalCount: requireUInt64(report.refusalCount, "refusal_count"),
      };
    }
    lastState = candidate
      ? `routable=${routableFps.includes(worker.workerFp)} capacity=${report ? "present" : "missing"}`
      : "worker absent";
    await delay(200);
  }
  throw new ScaleCapacityPreflightError(
    `worker ${worker.label} (${worker.workerFp}) did not report terminal-core capacity within ${timeoutMs}ms: ${lastState}`,
  );
}

export function assertFleetCapacity(
  reports: readonly ScaleWorkerCapacity[],
  requiredSessions: number,
): void {
  const available = reports.reduce((total, report) => total + report.available, 0);
  if (available >= requiredSessions) return;
  const detail = reports.map((report) =>
    `${report.label}:${report.used}+${report.pending}/${report.capacity} available=${report.available}`,
  ).join(", ");
  throw new ScaleCapacityPreflightError(
    `terminal-core capacity preflight needs ${requiredSessions} free cores but fleet reports ${available}: ${detail}`,
  );
}

function parseLimit(value: string | undefined): bigint | "unlimited" | null {
  if (!value) return null;
  if (value === "unlimited" || value === "max") return "unlimited";
  return /^\d+$/.test(value) ? BigInt(value) : null;
}

function processLimit(name: string): bigint | "unlimited" | null {
  if (process.platform !== "linux") return null;
  try {
    const line = readFileSync("/proc/self/limits", "utf8").split("\n")
      .find((candidate) => candidate.startsWith(name));
    if (!line) return null;
    const remainder = line.slice(name.length).trim().split(/\s+/);
    return parseLimit(remainder[0]);
  } catch {
    return null;
  }
}

function cgroupRoot(): string | null {
  if (process.platform !== "linux") return null;
  try {
    const line = readFileSync("/proc/self/cgroup", "utf8").split("\n")
      .find((candidate) => candidate.startsWith("0::"));
    if (!line) return null;
    const relative = line.slice(3).trim();
    return join("/sys/fs/cgroup", relative.startsWith("/") ? relative.slice(1) : relative);
  } catch {
    return null;
  }
}

function cgroupValue(root: string | null, file: string): bigint | "unlimited" | null {
  if (!root) return null;
  try {
    return parseLimit(readFileSync(join(root, file), "utf8").trim());
  } catch {
    return null;
  }
}

/** The local stack's workers inherit this process's cgroup and rlimits. */
export function preflightSoakResources(sessionCount: number): ScaleResourcePreflight {
  if (!Number.isSafeInteger(sessionCount) || sessionCount <= 0) {
    throw new ScaleCapacityPreflightError("soak session count must be a positive safe integer");
  }
  const root = cgroupRoot();
  const cgroupPidsCurrentValue = cgroupValue(root, "pids.current");
  const cgroupPidsCurrent = typeof cgroupPidsCurrentValue === "bigint"
    ? cgroupPidsCurrentValue
    : null;
  const result: ScaleResourcePreflight = {
    requiredReservedBytes: BigInt(sessionCount) * RESERVED_BYTES_PER_SESSION,
    openFiles: processLimit("Max open files"),
    maxProcesses: processLimit("Max processes"),
    cgroupMemoryHigh: cgroupValue(root, "memory.high"),
    cgroupMemoryMax: cgroupValue(root, "memory.max"),
    cgroupPidsCurrent,
    cgroupPidsMax: cgroupValue(root, "pids.max"),
  };
  const failures: string[] = [];
  if (typeof result.openFiles === "bigint" && result.openFiles < MINIMUM_OPEN_FILES) {
    failures.push(`open-file limit ${result.openFiles} is below ${MINIMUM_OPEN_FILES}`);
  }
  if (typeof result.maxProcesses === "bigint" && result.maxProcesses < MINIMUM_TASKS) {
    failures.push(`process limit ${result.maxProcesses} is below ${MINIMUM_TASKS}`);
  }
  for (const [name, value] of [
    ["cgroup memory.high", result.cgroupMemoryHigh],
    ["cgroup memory.max", result.cgroupMemoryMax],
  ] as const) {
    if (typeof value === "bigint" && value < result.requiredReservedBytes) {
      failures.push(`${name} ${value} is below ${result.requiredReservedBytes} reserved bytes`);
    }
  }
  if (typeof result.cgroupPidsMax === "bigint"
    && result.cgroupPidsCurrent !== null
    && result.cgroupPidsCurrent + BigInt(sessionCount) + PID_HEADROOM > result.cgroupPidsMax) {
    failures.push(
      `cgroup pids ${result.cgroupPidsCurrent}/${result.cgroupPidsMax} cannot admit ${sessionCount} sessions with ${PID_HEADROOM} headroom`,
    );
  }
  if (failures.length > 0) throw new ScaleCapacityPreflightError(failures.join("; "));
  return result;
}
