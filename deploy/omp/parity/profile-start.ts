import { appendFileSync, existsSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import type { ProfileManifest } from "./profile-manifest";

const workspace = process.env.OMP_PARITY_WORKSPACE;
const sourceRoot = process.env.OMP_PARITY_SOURCE_ROOT;
const entry = process.argv[1];
// Bun also reads local preload configuration for tool subprocesses; only the real CLI is a generation.
if (workspace && sourceRoot && entry && existsSync(entry) && realpathSync(entry) === realpathSync(join(sourceRoot, "packages/coding-agent/src/cli.ts"))) {
  const profile = JSON.parse(readFileSync(join(workspace, ".parity-profile.json"), "utf8")) as ProfileManifest;
  if (resolve(process.env.HOME ?? "") !== profile.home || resolve(profile.workspace) !== resolve(workspace)) throw new Error("Process observer requires the owned CLI profile");
  const counter = join(profile.home, ".parity/process-generation.json");
  const previous = existsSync(counter) ? JSON.parse(readFileSync(counter, "utf8")) as { generation: number } : { generation: 0 };
  if (!Number.isSafeInteger(previous.generation) || previous.generation < 0) throw new Error("Invalid real CLI generation record");
  const generation = previous.generation + 1;
  const launcherPid = Number(process.env.OMP_PARITY_LAUNCHER_PID);
  if (!Number.isSafeInteger(launcherPid) || launcherPid < 2) throw new Error("Real CLI launcher identity is missing");
  writeFileSync(counter, JSON.stringify({ generation, pid: process.pid, launcherPid, workspace, sourceRoot }));
  const trace = profile.environment.OMP_PARITY_PROCESS_TRACE;
  if (trace !== join(profile.home, ".parity/process.jsonl")) throw new Error("Process trace is not the owned profile trace");
  appendFileSync(trace, `${JSON.stringify({ channel: "builtin.process", value: { exited: false, restarted: generation > 1, generation, pid: process.pid, launcherPid } })}\n`);
}
