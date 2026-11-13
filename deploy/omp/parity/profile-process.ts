import { realpath } from "node:fs/promises";
import { join, resolve } from "node:path";
import { eventually, execute } from "./runtime";
import { assertOwned } from "./profile-manifest";

export interface ProcessGeneration { generation: number; pid: number; launcherPid: number; sourceRoot: string; workspace: string }

function alive(pid: number): boolean {
  if (!Number.isSafeInteger(pid) || pid < 2 || pid === process.pid) throw new Error("Invalid owned runtime PID");
  try { process.kill(pid, 0); return true; }
  catch (error) { if (error instanceof Error && "code" in error && error.code === "ESRCH") return false; throw error; }
}

async function assertIdentity(pid: number, entry: string, workspace: string, environment: Record<string, string>): Promise<void> {
  if (!Number.isSafeInteger(pid) || pid < 2 || pid === process.pid) throw new Error("Invalid owned runtime PID");
  const command = await execute(["ps", "-p", String(pid), "-o", "command="], workspace, environment);
  if (!command.includes(entry)) throw new Error(`Refusing to signal PID ${pid}: real command does not identify ${entry}`);
  const cwd = process.platform === "linux"
    ? await realpath(`/proc/${pid}/cwd`)
    : (await execute(["lsof", "-a", "-p", String(pid), "-d", "cwd", "-Fn"], workspace, environment)).split("\n").find(line => line.startsWith("n"))?.slice(1);
  if (!cwd) throw new Error(`Refusing to signal PID ${pid}: actual CWD is unavailable`);
  assertOwned(await realpath(workspace), await realpath(cwd));
}

export async function stopProfileRuntime(repoRoot: string, sourceRoot: string, workspace: string, home: string, environment: Record<string, string>): Promise<void> {
  const counter = Bun.file(join(home, ".parity/process-generation.json"));
  if (!await counter.exists()) return;
  const generation = await counter.json() as ProcessGeneration;
  if (resolve(generation.workspace) !== resolve(workspace) || resolve(generation.sourceRoot) !== resolve(sourceRoot)) throw new Error("Runtime generation is not owned by this profile");
  const sourceEntry = join(sourceRoot, "packages/coding-agent/src/cli.ts");
  const launcherEntry = join(repoRoot, "deploy/omp/parity/launch.ts");
  const processes = new Map<number, string>([[generation.pid, sourceEntry], [generation.launcherPid, launcherEntry]]);
  async function refreshSources(): Promise<void> {
    const rows = (await Bun.file(join(home, ".parity/process.jsonl")).text()).split("\n").filter(Boolean);
    for (const line of rows) {
      const row = JSON.parse(line) as { channel?: string; value?: { exited?: boolean; pid?: number; launcherPid?: number } };
      if (row.channel !== "builtin.process" || row.value?.exited !== false || row.value.pid === undefined) continue;
      processes.set(row.value.pid, sourceEntry);
      if (row.value.launcherPid !== undefined) processes.set(row.value.launcherPid, launcherEntry);
    }
  }
  await refreshSources();
  const live = [...processes].filter(([pid]) => alive(pid));
  if (live.length === 0) return;
  for (const [pid, entry] of live) await assertIdentity(pid, entry, workspace, environment);
  if (alive(generation.launcherPid)) process.kill(generation.launcherPid, "SIGTERM");
  else for (const [pid, entry] of live) if (entry === sourceEntry) process.kill(pid, "SIGTERM");
  const settled = async () => {
    await refreshSources();
    return [...processes.keys()].every(pid => !alive(pid)) ? true : undefined;
  };
  try {
    await eventually("actual owned CLI shutdown", settled, 10_000);
  } catch (error) {
    await refreshSources();
    for (const [pid, entry] of processes) {
      if (alive(pid)) { await assertIdentity(pid, entry, workspace, environment); process.kill(pid, "SIGKILL"); }
    }
    await eventually("actual killed CLI exit", settled, 5_000);
    throw new Error("Owned CLI did not complete graceful shutdown before forced teardown", { cause: error });
  }
}
