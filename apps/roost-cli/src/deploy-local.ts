import { run, resolveLocalGitShaOrDie } from "./deploy-exec.ts";
import { _backfillEnvFromPlist, _resolveDeployEnvValue } from "./deploy-plist-env.ts";
import { restartWorkerCmd, verifyWorkerCmd, WORKER_AGENT, WORKER_UNIT, type ServiceOs } from "./service-ctl.ts";

/** Localhost fast-path: run install.sh directly on the box that
 *  initiated the deploy, no ssh + no rsync (the source is already
 *  here). Reuses the same dirty-tree guard + git_sha logic the remote
 *  path uses so the resulting plist stamp is identical. */
export async function _deployLocal(host: string): Promise<void> {
  console.log(`>> local deploy on ${host} (source already in place — skipping ssh + rsync)`);

  // Dirty guard — the same one the remote path uses, so a sb31 violation
  // hits the user identically regardless of which host they target.
  const localGitSha = resolveLocalGitShaOrDie();

  console.log(`>> verify bun`);
  console.log(`   bun: ${process.execPath}`);

  const { env: hostEnv, filled } = await _backfillEnvFromPlist("self");
  if (filled.length > 0) {
    console.log(`>> reused from existing plist: ${filled.join(", ")}`);
  }
  const installEnv: Record<string, string> = {};
  for (const key of [
    "ROOST_COORDINATOR_URL",
    "ROOST_BOOTSTRAP_TOKEN",
    "ROOST_WORKER_LABEL",
    "ROOST_REACHABLE_ADDR",
  ]) {
    const value = _resolveDeployEnvValue(key, hostEnv);
    if (value) installEnv[key] = value;
  }
  if (!installEnv.ROOST_COORDINATOR_URL) {
    console.error("ERROR: ROOST_COORDINATOR_URL env var required (no prior plist to reuse).");
    process.exit(6);
  }

  const os: ServiceOs = process.platform === "linux" ? "linux" : "darwin";
  const service = os === "linux" ? WORKER_UNIT : WORKER_AGENT;
  console.log(`>> install worker service (${service}) locally`);
  const installSh = await run(
    // Absolute path from this module (../../../ = repo root) so it resolves no
    // matter the process cwd — the `roost` npm alias runs with cwd apps/roost-cli.
    ["bash", new URL("../../../apps/worker/scripts/install.sh", import.meta.url).pathname, "install"],
    {
      quiet: true,
      env: {
        ...installEnv,
        BUN_BIN: process.execPath,
        ...(localGitSha ? { GIT_SHA: localGitSha } : {}),
      },
    },
  );
  if (installSh.exit !== 0) {
    console.error("install.sh failed:");
    console.error(installSh.stdout);
    console.error(installSh.stderr);
    process.exit(5);
  }
  console.log(installSh.stdout.trim().split("\n").map((l) => `   ${l}`).join("\n"));

  console.log(`>> restart ${service} locally`);
  const kick = await run(["bash", "-c", restartWorkerCmd(os)], { quiet: true });
  if (kick.exit !== 0) {
    console.error(`restart failed (exit ${kick.exit}):`);
    console.error(kick.stdout);
    console.error(kick.stderr);
    process.exit(4);
  }

  console.log(`>> verifying service is up locally`);
  const { promise: settled, resolve: markSettled } = Promise.withResolvers<void>();
  setTimeout(markSettled, 1500);
  await settled;
  const verify = await run(["bash", "-c", verifyWorkerCmd(os)], { quiet: true });
  console.log(verify.stdout.trim().split("\n").map((l) => `   ${l}`).join("\n"));

  console.log(`>> done — ${host} v2 worker deployed (local)`);
}
