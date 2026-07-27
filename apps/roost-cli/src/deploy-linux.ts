// `roost deploy <linux-host>` — update an in-place git checkout instead of
// rsyncing a slim tree. A Linux worker is enrolled by join.sh, which clones
// the repo (default /srv/roost) and pins it to the coordinator's sha, so the
// box already has the full source + .git. Updating it is: fetch, checkout the
// local HEAD sha, bun install, re-run install.sh, verify the systemd unit.
//
// No `tailscale cert` step: the worker has had no inbound TLS surface since
// phase-25e, and no rsync: the checkout is the source of truth.

import { sshExec } from "./deploy-exec.ts";
import { verifyWorkerCmd, WORKER_UNIT } from "./service-ctl.ts";

const REMOTE_REPO = process.env.ROOST_LINUX_REPO_DIR ?? "/srv/roost";

export async function deployLinux(
  host: string,
  opts: { gitSha: string; passthroughEnv: string },
): Promise<void> {
  const { gitSha, passthroughEnv } = opts;

  // The remote clone pulls from GitHub, so a sha that exists only in the
  // local repo can never be checked out there. Fail before touching the box.
  if (!gitSha || gitSha.endsWith("-dirty")) {
    console.error("ERROR: a Linux deploy checks out a sha on the remote clone — it cannot ship a dirty tree.");
    console.error("  Commit and push first.");
    process.exit(7);
  }
  const contains = Bun.spawnSync(["git", "branch", "-r", "--contains", gitSha]);
  if (contains.exitCode !== 0 || contains.stdout.toString().trim().length === 0) {
    console.error(`ERROR: ${gitSha.slice(0, 8)} is not on any remote branch — push first.`);
    console.error("  git push origin HEAD");
    process.exit(7);
  }

  console.log(`>> checkout ${gitSha.slice(0, 8)} in ${host}:${REMOTE_REPO}`);
  const checkout = await sshExec(
    host,
    // --force: `bun install` on the box can touch tracked bun.lock, which
    // would make a plain checkout refuse on the next deploy.
    `git -C ${REMOTE_REPO} fetch --quiet origin && git -C ${REMOTE_REPO} checkout --quiet --force --detach ${gitSha} 2>&1`,
  );
  if (checkout.exit !== 0) {
    console.error("git checkout failed:");
    console.error(checkout.stdout);
    console.error(checkout.stderr);
    process.exit(2);
  }

  console.log(`>> bun install on ${host}`);
  const install = await sshExec(host, `set -eo pipefail; cd ${REMOTE_REPO} && bun install 2>&1 | tail -25`);
  if (install.exit !== 0) {
    console.error("bun install failed:");
    console.error(install.stdout);
    console.error(install.stderr);
    process.exit(4);
  }
  console.log("   bun install ok");

  console.log(`>> install systemd unit (${WORKER_UNIT}) on ${host}`);
  // install.sh resolves GIT_SHA from the checkout itself; the passthrough
  // keeps it identical to the macOS path (and covers a detached HEAD whose
  // rev-parse the script can't reach).
  const installSh = await sshExec(
    host,
    `${passthroughEnv} bash ${REMOTE_REPO}/apps/worker/scripts/install.sh install 2>&1`,
  );
  if (installSh.exit !== 0) {
    console.error("install.sh failed:");
    console.error(installSh.stdout);
    console.error(installSh.stderr);
    process.exit(5);
  }
  console.log(installSh.stdout.trim().split("\n").map((l) => `   ${l}`).join("\n"));

  console.log(`>> verifying service is up on ${host}`);
  const verify = await sshExec(host, verifyWorkerCmd("linux"));
  console.log(verify.stdout.trim().split("\n").map((l) => `   ${l}`).join("\n"));

  console.log(`>> done — ${host} v2 worker deployed (linux)`);
}
