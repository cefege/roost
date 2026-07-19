import { run } from "./deploy-exec.ts";
import { _backfillEnvFromPlist } from "./deploy-plist-env.ts";

/** Localhost fast-path: run install.sh directly on the box that
 *  initiated the deploy, no ssh + no rsync (the source is already
 *  here). Reuses the same dirty-tree guard + git_sha logic the remote
 *  path uses so the resulting plist stamp is identical. */
export async function _deployLocal(host: string): Promise<void> {
  console.log(`>> local deploy on ${host} (source already in place — skipping ssh + rsync)`);

  // Dirty guard — matches the remote path so a sb31 violation hits
  // the user the same way regardless of which host they target.
  let localGitSha = "";
  try {
    const r = Bun.spawnSync(["git", "rev-parse", "HEAD"]);
    if (r.exitCode === 0) localGitSha = r.stdout.toString().trim();
  } catch (e) { console.error("git check failed:", String(e)); }
  let isDirty = false;
  try {
    const st = Bun.spawnSync(["git", "status", "--porcelain"]);
    if (st.exitCode === 0 && st.stdout.toString().trim().length > 0) isDirty = true;
  } catch (e) { console.error("git check failed:", String(e)); }
  if (isDirty) {
    if (process.env.ROOST_ALLOW_DIRTY === "1") {
      console.warn(`>> WARN: uncommitted changes — stamping GIT_SHA=${localGitSha}-dirty (ROOST_ALLOW_DIRTY=1)`);
      localGitSha = localGitSha ? `${localGitSha}-dirty` : "";
    } else {
      console.error("ERROR: uncommitted changes in working tree.");
      console.error("  Commit first, OR re-run with ROOST_ALLOW_DIRTY=1");
      process.exit(7);
    }
  }

  console.log(`>> verify bun`);
  console.log(`   bun: ${process.execPath}`);

  const filled = await _backfillEnvFromPlist("self");
  if (filled.length > 0) {
    console.log(`>> reused from existing plist: ${filled.join(", ")}`);
  }
  if (!process.env.ROOST_COORDINATOR_URL) {
    console.error("ERROR: ROOST_COORDINATOR_URL env var required (no prior plist to reuse).");
    process.exit(6);
  }

  console.log(`>> install LaunchAgent (com.roost.worker-v2) locally`);
  const installSh = await run(
    // Absolute path from this module (../../../ = repo root) so it resolves no
    // matter the process cwd — the `roost` npm alias runs with cwd apps/roost-cli.
    ["bash", new URL("../../../apps/worker/scripts/install.sh", import.meta.url).pathname, "install"],
    {
      quiet: true,
      env: {
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

  console.log(`>> kickstart com.roost.worker-v2 locally`);
  const kick = await run(["launchctl", "kickstart", "-k", `gui/${process.getuid?.() ?? ""}/com.roost.worker-v2`], { quiet: true });
  if (kick.exit !== 0) {
    console.error(`kickstart failed (exit ${kick.exit}):`);
    console.error(kick.stdout);
    console.error(kick.stderr);
    process.exit(4);
  }

  console.log(`>> verifying service is up locally`);
  const { promise: settled, resolve: markSettled } = Promise.withResolvers<void>();
  setTimeout(markSettled, 1500);
  await settled;
  const verify = await run(
    ["bash", "-c", `launchctl print gui/$(id -u)/com.roost.worker-v2 2>&1 | grep -E '^\\s*(state|pid|active count)' | head -5`],
    { quiet: true },
  );
  console.log(verify.stdout.trim().split("\n").map((l) => `   ${l}`).join("\n"));

  console.log(`>> done — ${host} v2 worker deployed (local)`);
}
