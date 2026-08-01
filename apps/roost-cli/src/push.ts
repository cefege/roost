// `roost push` — one-shot release: git push, then redeploy every machine in
// the fleet (local + remote tailnet workers), then restart the local
// coord. Closes the failure mode where remote `roost deploy` succeeded
// but the local box kept running stale code for hours until someone
// happened to run install.sh by hand — the "remote works, local broken"
// class that surfaced as "Create folder failed: [internal] internal
// error" once the wire schemas drifted.
//
// Targets come from --targets= or ROOST_PUSH_TARGETS (comma-sep tailnet
// labels), set in .env.local — no hardcoded fleet. Unset → fail loud (see
// reference_roost_deploy_targets.md).

import { join } from "node:path";
import { deploy } from "./deploy.ts";
import { currentServiceOs, restartCoordCmd } from "./service-ctl.ts";

// Repo root = three levels up from apps/roost-cli/src/push.ts.
const REPO_ROOT = join(import.meta.dir, "..", "..", "..");

async function run(cmd: string[], cwd?: string): Promise<number> {
  const proc = Bun.spawn({ cmd, cwd, stdio: ["inherit", "inherit", "inherit"] });
  await proc.exited;
  return proc.exitCode ?? 1;
}

export async function push(args: string[]): Promise<void> {
  const skipGit = args.includes("--no-git");
  const skipLocalCoord = args.includes("--no-coord");
  const targetsArg = args.find((a) => a.startsWith("--targets="));
  const targetsRaw = targetsArg ? targetsArg.slice("--targets=".length) : process.env.ROOST_PUSH_TARGETS;
  const targets = (targetsRaw ?? "").split(",").map((s) => s.trim()).filter(Boolean);
  if (targets.length === 0) {
    console.error("no push targets: set ROOST_PUSH_TARGETS (comma-sep tailnet hostnames) in .env.local, or pass --targets=host1,host2");
    process.exit(2);
  }

  if (!skipGit) {
    console.log(">> git push");
    const code = await run(["git", "push"]);
    if (code !== 0) {
      console.error(`git push failed (exit ${code}); aborting`);
      process.exit(code);
    }
  } else {
    console.log(">> skipping git push (--no-git)");
  }

  // Workers in parallel would race rsync against ssh handshakes; serial
  // keeps the log readable and dirty-tree errors don't compete.
  for (const host of targets) {
    console.log(`\n>> roost deploy ${host}`);
    try {
      await deploy([host]);
    } catch (e) {
      console.error(`deploy ${host} failed: ${e instanceof Error ? e.message : String(e)}`);
      console.error(">> continuing with remaining targets");
    }
  }

  // Worker service's deploy step already restarted itself for the
  // local target inside `_deployLocal`. Coord is the one piece deploy
  // never touches — it has its own launchd/systemd service + install.sh.
  // Kick it here so local coord + local worker advance to the same git SHA
  // the remote workers just received.
  // Build the SPA so coord serves the pushed commit. Without this, coord
  // kickstart re-reads its own git sha but keeps serving a stale apps/web/dist
  // → the SPA's "new version — reload" nudge fires forever (reloading can't
  // fix a dist nobody rebuilt). Baked VITE_BUILD_SHA then matches coord's sha.
  if (!skipLocalCoord && !args.includes("--no-web")) {
    console.log("\n>> build SPA (apps/web → dist)");
    const code = await run(["bun", "run", "build"], join(REPO_ROOT, "apps", "web"));
    if (code !== 0) {
      console.error(`SPA build failed (exit ${code}); aborting before coord kickstart`);
      process.exit(code);
    }
  }

  if (!skipLocalCoord) {
    console.log("\n>> restart local coord");
    await run(["bash", "-c", restartCoordCmd(currentServiceOs())]);
  } else {
    console.log("\n>> skipping local coord kickstart (--no-coord)");
  }

  console.log("\n>> push complete");
}
