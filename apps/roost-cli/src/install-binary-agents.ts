// Install the coord/worker LaunchAgents for the COMPILED binary. Extracts the
// embedded install scripts (baked by scripts/gen-embed.ts) to a temp dir and
// runs them with ROOST_EXEC_BIN=<this binary> so the plists launch
// `roost coord` / `roost worker` instead of `bun …/main.ts`. This REUSES the
// exact bash (FRONTED, TLS detection, tailscale serve, launchctl bootstrap) —
// no reimplementation of the safety-critical env. `cmd` "install" boots the
// agents; "write-plist" only generates the plist into a throwaway temp dir
// (dry-run — never touches ~/Library/LaunchAgents, no launchctl).
import { COORD_INSTALL_SH, WORKER_INSTALL_SH } from "@roost/shared/install-scripts";
import { mkdtempSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

type Cmd = "install" | "write-plist";

function extractScript(name: string, body: string): string {
  if (!body) {
    throw new Error("embedded install scripts missing — build with scripts/build-binary.ts");
  }
  const dir = mkdtempSync(join(tmpdir(), "roost-agents-"));
  const path = join(dir, name);
  writeFileSync(path, body);
  return path;
}

async function runScript(path: string, cmd: Cmd, env: Record<string, string>): Promise<void> {
  const proc = Bun.spawn(["bash", path, cmd], {
    env: { ...process.env, ...env },
    stdio: ["ignore", "inherit", "inherit"],
  });
  if ((await proc.exited) !== 0) throw new Error(`${path} ${cmd} failed`);
}

export async function installCoordAgent(opts: {
  execPath: string; gitSha: string; cmd?: Cmd; log: (m: string) => void;
}): Promise<void> {
  const cmd = opts.cmd ?? "install";
  opts.log(`installing coordinator LaunchAgent (roost coord)${cmd === "write-plist" ? " [dry-run]" : ""}`);
  const script = extractScript("coord-install.sh", COORD_INSTALL_SH);
  const env: Record<string, string> = {
    ROOST_EXEC_BIN: opts.execPath,
    ROOST_WORKDIR: homedir(),
    ROOST_GIT_SHA: opts.gitSha, // coord install.sh reads ROOST_GIT_SHA
  };
  if (cmd === "write-plist") {
    // Force everything into a throwaway dir so dry-run can NEVER overwrite the
    // real ~/Library/LaunchAgents plist, regardless of the caller's env.
    const dir = mkdtempSync(join(tmpdir(), "roost-dryrun-coord-"));
    env.ROOST_COORD_LABEL = "com.roost.coordinator-dryrun";
    env.ROOST_COORD_PLIST = join(dir, "coord.plist");
    env.ROOST_COORD_DATA_DIR = join(dir, "data");
    env.ROOST_COORD_LOG_DIR = join(dir, "logs");
    opts.log(`  dry-run plist → ${env.ROOST_COORD_PLIST}`);
  }
  await runScript(script, cmd, env);
}

export async function installWorkerAgent(opts: {
  execPath: string; coordUrl: string; bootstrapToken?: string; gitSha: string; cmd?: Cmd; log: (m: string) => void;
}): Promise<void> {
  const cmd = opts.cmd ?? "install";
  opts.log(`installing worker LaunchAgent (roost worker)${cmd === "write-plist" ? " [dry-run]" : ""}`);
  const script = extractScript("worker-install.sh", WORKER_INSTALL_SH);
  const env: Record<string, string> = {
    ROOST_EXEC_BIN: opts.execPath,
    ROOST_WORKDIR: homedir(),
    ROOST_COORDINATOR_URL: opts.coordUrl,
    GIT_SHA: opts.gitSha, // worker install.sh reads GIT_SHA (not ROOST_GIT_SHA)
    ...(opts.bootstrapToken ? { ROOST_BOOTSTRAP_TOKEN: opts.bootstrapToken } : {}),
  };
  if (cmd === "write-plist") {
    const dir = mkdtempSync(join(tmpdir(), "roost-dryrun-worker-"));
    env.ROOST_WORKER_AGENT_LABEL = "com.roost.worker-dryrun";
    env.ROOST_WORKER_PLIST = join(dir, "worker.plist");
    env.ROOST_WORKER_DATA_DIR = join(dir, "data");
    env.ROOST_WORKER_LOG_DIR = join(dir, "logs");
    opts.log(`  dry-run plist → ${env.ROOST_WORKER_PLIST}`);
  }
  await runScript(script, cmd, env);
}
