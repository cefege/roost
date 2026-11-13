import type { ParityCase } from "./cases";
import type { RunnerShard } from "./runner-options";

// Canonical OMP listeners bind fixed loopback ports (Codex OAuth callback,
// stats dashboard) and refuse an occupied port, so cases that declare one
// must never overlap; every other case is fully isolated and may run beside
// any other.
const portKey = /port$/i;

function collectPorts(value: unknown, ports: Set<number>, underPorts = false): void {
  if (Array.isArray(value)) { for (const item of value) collectPorts(item, ports, underPorts); return; }
  if (typeof value !== "object" || value === null) return;
  for (const [key, entry] of Object.entries(value)) {
    if ((portKey.test(key) || underPorts) && typeof entry === "number" && Number.isInteger(entry) && entry > 0 && entry < 65536) ports.add(entry);
    else collectPorts(entry, ports, key === "ports");
  }
}

export function exclusivePorts(item: ParityCase): number[] {
  if (item.kind !== "builtin") return [];
  const ports = new Set<number>();
  collectPorts(item.requirements, ports);
  return [...ports].sort((left, right) => left - right);
}

export function shardSelection<T>(items: readonly T[], shard: RunnerShard | undefined): T[] {
  return shard ? items.filter((_, index) => index % shard.count === shard.index - 1) : [...items];
}

export interface ScheduledItem { index: number; item: ParityCase }

export async function runScheduled<R>(items: readonly ScheduledItem[], jobs: number, signal: AbortSignal,
  runOne: (scheduled: ScheduledItem) => Promise<R>, stop: (result: R) => boolean): Promise<R[]> {
  const pending = items.map(scheduled => ({ scheduled, ports: exclusivePorts(scheduled.item) }));
  const held = new Set<number>();
  const results: Array<{ index: number; result: R }> = [];
  let active = 0;
  let halted = false;
  const settled = Promise.withResolvers<void>();
  const failure = (error: unknown) => { halted = true; settled.reject(error); };
  const launch = (): void => {
    while (!halted && !signal.aborted && active < jobs) {
      const next = pending.findIndex(entry => entry.ports.every(port => !held.has(port)));
      if (next < 0) break;
      const [entry] = pending.splice(next, 1);
      if (!entry) break;
      for (const port of entry.ports) held.add(port);
      active++;
      runOne(entry.scheduled).then(result => {
        results.push({ index: entry.scheduled.index, result });
        if (stop(result)) halted = true;
      }, failure).finally(() => {
        for (const port of entry.ports) held.delete(port);
        active--;
        launch();
      });
    }
    if (active === 0) settled.resolve();
  };
  launch();
  await settled.promise;
  return results.sort((left, right) => left.index - right.index).map(entry => entry.result);
}
