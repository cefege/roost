// Install the coord/worker services for the COMPILED binary. Extracts the
// embedded install scripts (baked by scripts/gen-embed.ts) to a temp dir and
// runs them with ROOST_EXEC_BIN=<this binary> so the service definitions launch
// `roost coord` / `roost worker` instead of `bun …/main.ts`. This REUSES the
// exact bash (FRONTED, TLS detection, tailscale serve, launchd/systemd
// bootstrap) — no reimplementation of the safety-critical env. `cmd` "install"
// boots the services; "write-plist" only generates the service definition into
// a throwaway temp dir (dry-run — never touches ~/Library/LaunchAgents or
// ~/.config/systemd/user, no launchctl/systemctl).
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
  opts.log(`installing coordinator service (roost coord)${cmd === "write-plist" ? " [dry-run]" : ""}`);
  const script = extractScript("coord-install.sh", COORD_INSTALL_SH);
  const env: Record<string, string> = {
    ROOST_EXEC_BIN: opts.execPath,
    ROOST_WORKDIR: homedir(),
    ROOST_GIT_SHA: opts.gitSha, // coord install.sh reads ROOST_GIT_SHA
  };
  if (cmd === "write-plist") {
    // Force everything into a throwaway dir so dry-run can NEVER overwrite the
    // real service definition, regardless of the caller's env. Both the darwin
    // (*_PLIST) and linux (*_UNIT) knobs must be set: the installer's Linux
    // branch ignores ROOST_COORD_PLIST entirely.
    const dir = mkdtempSync(join(tmpdir(), "roost-dryrun-coord-"));
    env.ROOST_COORD_LABEL = "com.roost.coordinator-dryrun";
    env.ROOST_COORD_PLIST = join(dir, "coord.plist");
    env.ROOST_COORD_UNIT = join(dir, "coord.service");
    env.ROOST_COORD_DATA_DIR = join(dir, "data");
    env.ROOST_COORD_LOG_DIR = join(dir, "logs");
    opts.log(`  dry-run service definition → ${process.platform === "darwin" ? env.ROOST_COORD_PLIST : env.ROOST_COORD_UNIT}`);
  }
  await runScript(script, cmd, env);
}

export async function installWorkerAgent(opts: {
  execPath: string; coordUrl: string; bootstrapToken?: string; gitSha: string; cmd?: Cmd; log: (m: string) => void;
}): Promise<void> {
  const cmd = opts.cmd ?? "install";
  opts.log(`installing worker service (roost worker)${cmd === "write-plist" ? " [dry-run]" : ""}`);
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
    env.ROOST_WORKER_UNIT = join(dir, "worker.service");
    env.ROOST_WORKER_DATA_DIR = join(dir, "data");
    env.ROOST_WORKER_LOG_DIR = join(dir, "logs");
    opts.log(`  dry-run service definition → ${process.platform === "darwin" ? env.ROOST_WORKER_PLIST : env.ROOST_WORKER_UNIT}`);
  }
  await runScript(script, cmd, env);
}
