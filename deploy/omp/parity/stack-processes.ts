import { readFile, stat } from "node:fs/promises";
import { eventually, execute, type RunningChild } from "./runtime";

export interface StackProcessIdentity {
  pid: number;
  parent: number;
  group: number;
  started: string;
  zombie: boolean;
}

async function processTable(cwd: string, env: Record<string, string>): Promise<Map<number, StackProcessIdentity>> {
  const stdout = await execute(["/bin/ps", "-axo", "pid=,ppid=,pgid=,stat=,lstart="], cwd, { ...env, LC_ALL: "C", TZ: "UTC" });
  const processes = new Map<number, StackProcessIdentity>();
  for (const line of stdout.trim().split("\n")) {
    const fields = /^\s*(\d+)\s+(\d+)\s+(\d+)\s+(\S+)\s+(.+)$/.exec(line);
    if (!fields) throw new Error("ps returned a malformed process identity row during stack cleanup");
    const [, pid, parent, group, state, started] = fields;
    if (!pid || !parent || !group || !state || !started) throw new Error("ps omitted process identity fields");
    processes.set(Number(pid), {
      pid: Number(pid), parent: Number(parent), group: Number(group), started, zombie: state.startsWith("Z"),
    });
  }
  return processes;
}

export async function captureStackRoot(pid: number, cwd: string, env: Record<string, string>): Promise<StackProcessIdentity> {
  if (!Number.isSafeInteger(pid) || pid <= 1 || pid === process.pid) throw new Error("invalid owned stack pid");
  const identity = (await processTable(cwd, env)).get(pid);
  if (!identity || identity.zombie) throw new Error(`owned stack process ${pid} exited before its identity was captured`);
  return identity;
}

export async function captureKeeperRoot(pidFile: string, cwd: string, env: Record<string, string>): Promise<StackProcessIdentity | undefined> {
  const [contents, file] = await Promise.all([readFile(pidFile, "utf8"), stat(pidFile)]);
  const value = contents.trim();
  const pid = Number(value);
  if (!/^[1-9]\d*$/.test(value) || !Number.isSafeInteger(pid) || pid <= 1 || pid === process.pid) {
    throw new Error("isolated keeper wrote an invalid pid file");
  }
  const identity = (await processTable(cwd, env)).get(pid);
  if (!identity || identity.zombie) return undefined;
  const startedMs = Date.parse(identity.started);
  if (!Number.isFinite(startedMs)) throw new Error("ps returned an unreadable keeper start time");
  if (startedMs > file.mtimeMs + 1_000) return undefined;
  return identity;
}

function stillOwned(identity: StackProcessIdentity, current: Map<number, StackProcessIdentity>): boolean {
  const live = current.get(identity.pid);
  return live !== undefined && !live.zombie && live.started === identity.started;
}

function signalOwned(identity: StackProcessIdentity, current: Map<number, StackProcessIdentity>, signal: NodeJS.Signals): void {
  // PID reuse must never turn fixture cleanup into a signal to an unrelated process.
  if (!stillOwned(identity, current)) return;
  try { process.kill(identity.pid, signal); }
  catch (error) {
    if (!(error instanceof Error && "code" in error && error.code === "ESRCH")) throw error;
  }
}

export async function stopStackProcesses(
  children: readonly RunningChild[],
  roots: readonly StackProcessIdentity[],
  cwd: string,
  env: Record<string, string>,
): Promise<void> {
  const failures: unknown[] = [];
  let owned = new Map<number, StackProcessIdentity>(roots.map(identity => [identity.pid, identity]));
  try {
    const table = await processTable(cwd, env);
    const validRoots = roots.filter(identity => !table.has(identity.pid) || stillOwned(identity, table));
    const groups = new Set(validRoots.filter(identity => identity.group === identity.pid).map(identity => identity.group));
    const ownedPids = new Set(validRoots.map(identity => identity.pid));
    let expanded = true;
    while (expanded) {
      expanded = false;
      for (const identity of table.values()) {
        if (!ownedPids.has(identity.pid) && (ownedPids.has(identity.parent) || groups.has(identity.group))) {
          ownedPids.add(identity.pid);
          expanded = true;
        }
      }
    }
    owned = new Map([...table].filter(([pid]) => ownedPids.has(pid)));
  } catch (error) { failures.push(error); }
  // Let worker shutdown settle ACP first; its keeper deliberately outlives the worker.
  for (const child of [...children].reverse()) {
    try { await child.stop(); }
    catch (error) { failures.push(error); }
  }
  try {
    let current = await processTable(cwd, env);
    for (const identity of owned.values()) signalOwned(identity, current, "SIGTERM");
    if (owned.size > 0) await Bun.sleep(200);
    current = await processTable(cwd, env);
    for (const identity of owned.values()) signalOwned(identity, current, "SIGKILL");
    await eventually("isolated stack process cleanup", async () => {
      const remaining = await processTable(cwd, env);
      const survivors = [...owned.values()].filter(identity => stillOwned(identity, remaining));
      if (survivors.length > 0) throw new Error(`owned processes remain: ${survivors.map(identity => identity.pid).join(", ")}`);
      return true;
    }, 5_000);
  } catch (error) { failures.push(error); }
  if (failures.length > 0) throw new AggregateError(failures, "could not completely clean up isolated Roost processes");
}
