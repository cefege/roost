// `roost deploy <host>` — refresh the v2 worker on a tailnet host.
//
// macOS target: rsync the worker tree, `bun install`, (re)install the
// LaunchAgent via apps/worker/scripts/install.sh, kickstart it. Idempotent —
// first deploy installs, subsequent deploys just refresh + kickstart.
//
// Layout on remote:
//   ~/RoostWorkerV2/
//     apps/worker/...        (rsynced)
//     apps/shared/...        (rsynced)
//     package.json + bun.lock
//     node_modules/          (bun install)
//
// Linux target: forks to deploy-linux.ts, which updates the git checkout
// join.sh left at /srv/roost and drives the systemd --user unit instead.
//
// LaunchAgent label: com.roost.worker-v2 (avoids collision with legacy
// com.roost.worker until R6.5).

import { existsSync } from "node:fs";
import { tailnetSuffix } from "./status.ts";
import { finishWorkerDeploy, run, runOrDie, SSH_OPTS, RSYNC_RSH, sshExec, resolveLocalGitShaOrDie } from "./deploy-exec.ts";
import { _isSelfHost } from "./deploy-self-host.ts";
import { _backfillEnvFromPlist, _resolveDeployEnvValue } from "./deploy-plist-env.ts";
import { _deployLocal } from "./deploy-local.ts";
import { deployLinux } from "./deploy-linux.ts";
import { verifyWorkerCmd } from "./service-ctl.ts";

// Re-export the public surface external modules import from ./deploy.ts —
// keeper-refresh.ts pulls { _isSelfHost, sshExec }; main/push/quickstart use deploy.
export { sshExec, _isSelfHost };

const REMOTE_DIR = "~/RoostWorkerV2";

/** Pick the FQDN used for a Mac worker certificate from the already-resolved
 *  target identity, falling back to the target hostname for a fresh install. */
export function _selectMacCertificateFqdn(
  host: string,
  resolvedReachableAddr: string | undefined,
  tailnetDnsSuffix: string | null | undefined,
): string | undefined {
  if (resolvedReachableAddr) return resolvedReachableAddr;
  if (!tailnetDnsSuffix) return undefined;
  return host.endsWith(`.${tailnetDnsSuffix}`) ? host : `${host}.${tailnetDnsSuffix}`;
}

/** Quote one value for a POSIX remote-shell assignment without expansion. */
export function _quoteRemoteShell(value: string): string {
  return `'${value.replaceAll("'", `'\"'\"'`)}'`;
}

/** Build the remote certificate command without a pipeline so the SSH result
 * is tailscale's exit status, not a trailing output filter's. */
export function _remoteCertCommand(remoteFqdn: string): string {
  const remoteTlsDir = "~/Library/Application\\ Support/RoostWorkerV2/tls";
  return `mkdir -p ${remoteTlsDir} && cd ${remoteTlsDir} && tailscale cert ${_quoteRemoteShell(remoteFqdn)} 2>&1`;
}

function remoteEnvAssignment(key: string, value: string): string {
  return `${key}=${_quoteRemoteShell(value)}`;
}

export async function deploy(args: string[]): Promise<void> {
  const host = args[0];
  if (!host) {
    console.error("usage: roost deploy <tailnet-host>");
    process.exit(1);
  }

  // Localhost fast-path: skip ssh + rsync entirely (the source IS
  // here), run install.sh directly with the same env we'd have shipped
  // remotely. Avoids the "Permission denied (publickey, ...)" SSH
  // failure when the local box doesn't trust its own pubkey.
  if (await _isSelfHost(host)) {
    await _deployLocal(host);
    return;
  }

  console.log(`>> reachability check ssh ${host}`);
  const ssh = await run(
    ["ssh", ...SSH_OPTS, "-o", "BatchMode=yes", host, "true"],
  );
  if (ssh.exit !== 0) {
    console.error("ssh failed; ensure key-based / tailscale-ssh auth");
    process.exit(2);
  }

  console.log(`>> verify bun on ${host}`);
  const bunCheck = await sshExec(host, "command -v bun && bun --version");
  if (bunCheck.exit !== 0) {
    console.error("bun not found in remote login shell. Install: curl -fsSL https://bun.sh/install | bash");
    console.error(`stderr: ${bunCheck.stderr}`);
    process.exit(3);
  }
  console.log(`   bun: ${bunCheck.stdout.trim().split("\n").slice(-2).join(" @ ")}`);

  // Linux targets hold a full git checkout (join.sh clones the repo), so
  // they update in place — no rsync of a slim tree, no tailscale cert.
  const unameOut = await sshExec(host, "uname -s");
  if (unameOut.stdout.trim() === "Linux") {
    const gitSha = resolveLocalGitShaOrDie();
    const { env: hostEnv, filled } = await _backfillEnvFromPlist(host);
    if (filled.length > 0) {
      console.log(`>> reused from the installed unit on ${host}: ${filled.join(", ")}`);
    }
    // Installed target identity wins; ambient values only seed fresh installs.
    const resolved = (k: string): string | undefined => _resolveDeployEnvValue(k, hostEnv);
    if (!resolved("ROOST_COORDINATOR_URL")) {
      console.error("ERROR: ROOST_COORDINATOR_URL env var required (no prior install on target to reuse).");
      process.exit(6);
    }
    const passthroughEnv = ["ROOST_COORDINATOR_URL", "ROOST_BOOTSTRAP_TOKEN", "ROOST_WORKER_LABEL", "ROOST_REACHABLE_ADDR"]
      .map((key) => [key, resolved(key)] as const)
      .filter((entry): entry is readonly [string, string] => entry[1] !== undefined)
      .map(([key, value]) => remoteEnvAssignment(key, value))
      .join(" ") + (gitSha ? ` ${remoteEnvAssignment("GIT_SHA", gitSha)}` : "");
    await deployLinux(host, { gitSha, passthroughEnv });
    return;
  }

  console.log(`>> ensure ${REMOTE_DIR}/ on ${host}`);
  await sshExec(host, `mkdir -p ${REMOTE_DIR}`);

  // Every worker ships the coordinator too: CoordTarget resolves the installer
  // and the SPA build against process.cwd(), so a Mac that only has
  // apps/worker can never accept a coordinator move. The Linux path already
  // deploys a full checkout — this keeps the two consistent.
  console.log(`>> rsync apps/{worker,shared,coord,web}/ + vendor/ → ${host}:${REMOTE_DIR}/`);
  await Promise.all([
    runOrDie([
      "rsync", "-az", "-e", RSYNC_RSH, "--delete",
      "--exclude", "node_modules", "--exclude", "tests", "--exclude", "test-results",
      "apps/worker/", `${host}:${REMOTE_DIR}/apps/worker/`,
    ], "rsync apps/worker"),
    runOrDie([
      "rsync", "-az", "-e", RSYNC_RSH, "--delete",
      "--exclude", "node_modules",
      "apps/shared/", `${host}:${REMOTE_DIR}/apps/shared/`,
    ], "rsync apps/shared"),
    runOrDie([
      "rsync", "-az", "-e", RSYNC_RSH, "--delete",
      "--exclude", "node_modules", "--exclude", "tests", "--exclude", "test-results",
      "apps/coord/", `${host}:${REMOTE_DIR}/apps/coord/`,
    ], "rsync apps/coord"),
    runOrDie([
      // dist is excluded on purpose: CoordTarget builds it on the target at
      // PREPARE, and a stale copy would be served in preference to that.
      "rsync", "-az", "-e", RSYNC_RSH, "--delete",
      "--exclude", "node_modules", "--exclude", "dist", "--exclude", "tests",
      "apps/web/", `${host}:${REMOTE_DIR}/apps/web/`,
    ], "rsync apps/web"),
    ...(existsSync("vendor")
      ? [runOrDie([
          "rsync", "-az", "-e", RSYNC_RSH, "--delete",
          "--exclude", "node_modules", "--exclude", "dist",
          "vendor/", `${host}:${REMOTE_DIR}/vendor/`,
        ], "rsync vendor")]
      : []),
    runOrDie([
      // Every apps/*/tsconfig.json extends this, and vite resolves the whole
      // extends chain (the @roost/* path aliases live in it) — without it the
      // target's SPA build dies before it emits anything.
      "rsync", "-az", "-e", RSYNC_RSH,
      "tsconfig.base.json", "bunfig.toml", `${host}:${REMOTE_DIR}/`,
    ], "rsync root config"),
  ]);

  // bun.lock from the dev tree was computed against the full workspace
  // set (apps/* + smoke). The deploy uses a slim package.json so the
  // hashes drift. Skip rsyncing bun.lock; bun install computes a fresh
  // one on the remote each deploy.

  console.log(`>> write deploy package.json (deps lifted from apps/worker/package.json)`);
  // The root package.json declares apps/* + smoke, but the deploy only
  // rsyncs apps/worker + apps/shared. Write a slim package.json on the
  // remote that lists only those, so `bun install` doesn't error on
  // missing workspaces. We also restate the worker's deps at the root
  // level so they resolve via the hoisted node_modules. (Historically
  // this was load-bearing for the @trpc subpath resolution; tRPC is
  // retired post-crpc6, but the hoisting still helps Connect-node + the
  // node-pty native binary find their peer modules consistently.) Read
  // the worker's package.json on the fly so versions never drift.
  const workerPkg = JSON.parse(
    await Bun.file("apps/worker/package.json").text(),
  ) as { dependencies?: Record<string, string> };
  const deployPackageJson = JSON.stringify({
    name: "roost-worker-deploy",
    private: true,
    type: "module",
    workspaces: ["apps/worker", "apps/shared", "apps/coord", "apps/web"],
    dependencies: workerPkg.dependencies ?? {},
    engines: { bun: ">=1.3.14" },
  }, null, 2);
  await sshExec(host, `cat > ${REMOTE_DIR}/package.json <<'PKG_EOF'\n${deployPackageJson}\nPKG_EOF`);

  // NOT --production: apps/web's build needs vite + vite-plugin-solid, which
  // are devDependencies. Without them a coordinator move fails at PREPARE.
  console.log(`>> bun install on ${host}`);
  const installRes = await sshExec(
    host,
    // `set -o pipefail` so the tail filter doesn't mask a non-zero exit.
    `set -eo pipefail; cd ${REMOTE_DIR} && bun install 2>&1 | tail -25`,
  );
  if (installRes.exit !== 0) {
    console.error("bun install failed:");
    console.error(installRes.stdout);
    console.error(installRes.stderr);
    process.exit(4);
  }
  console.log("   bun install ok");

  const { env: hostEnv, filled } = await _backfillEnvFromPlist(host);
  if (filled.length > 0) {
    console.log(`>> reused from existing plist on ${host}: ${filled.join(", ")}`);
  }
  const resolved = (k: string): string | undefined => _resolveDeployEnvValue(k, hostEnv);
  const resolvedCoordinatorUrl = resolved("ROOST_COORDINATOR_URL");
  if (!resolvedCoordinatorUrl) {
    console.error("ERROR: ROOST_COORDINATOR_URL env var required (no prior plist on target to reuse).");
    console.error("  ROOST_COORDINATOR_URL=https://<your-coord-host>:4102 \\");
    console.error("    ROOST_BOOTSTRAP_TOKEN=roost_bt_... \\");
    console.error(`    bun apps/roost-cli/src/main.ts deploy ${host}`);
    process.exit(6);
  }
  const resolvedReachableAddr = resolved("ROOST_REACHABLE_ADDR");

  // Mint a tailnet TLS cert on the remote so the worker can serve WSS to
  // browsers loading the SPA from https://<coord-fqdn>:4102/. `tailscale
  // cert` runs as the user (no sudo) on GUI Tailscale. The cert files
  // land in the worker's data dir, then ROOST_TLS_CERT_PATH / _KEY_PATH
  // are baked into the plist by install.sh.
  const certSuffix = resolvedReachableAddr ? null : tailnetSuffix();
  const remoteFqdn = _selectMacCertificateFqdn(host, resolvedReachableAddr, certSuffix);
  if (!remoteFqdn) {
    console.error(
      `cannot resolve tailnet suffix for ${host}: set ROOST_TAILNET_SUFFIX in .env.local, or start tailscale (\`tailscale up\`), or pass ROOST_REACHABLE_ADDR`,
    );
    process.exit(7);
  }
  // Keep service paths unescaped: they are data values, not shell fragments.
  const remoteCertPath = `~/Library/Application Support/RoostWorkerV2/tls/${remoteFqdn}.crt`;
  const remoteKeyPath = `~/Library/Application Support/RoostWorkerV2/tls/${remoteFqdn}.key`;
  console.log(`>> mint tailnet cert for ${remoteFqdn} on ${host}`);
  const certRes = await sshExec(
    host,
    _remoteCertCommand(remoteFqdn),
  );
  if (certRes.exit !== 0) {
    console.error("tailscale cert failed (worker will serve plain ws):");
    console.error(certRes.stdout);
    console.error(certRes.stderr);
    // Durable-signal bridge: coord's deploy-jobs.ts emitLine parses this
    // ROOST_SIGNAL sentinel into a `deploy.cert_skipped` signal (the child's
    // stderr only reaches an ephemeral bus otherwise).
    console.log(`ROOST_SIGNAL deploy.cert_skipped {"remote_fqdn":${JSON.stringify(remoteFqdn)}}`);
  } else {
    console.log(certRes.stdout.trim().split("\n").map((l) => `   ${l}`).join("\n"));
  }

  console.log(`>> install LaunchAgent (com.roost.worker-v2) on ${host}`);
  // Pass-through env: ROOST_COORDINATOR_URL is required for first deploy;
  // ROOST_BOOTSTRAP_TOKEN is one-shot for first deploy + cleared from
  // plist after redeem; ROOST_WORKER_LABEL / ROOST_REACHABLE_ADDR optional;
  // ROOST_TLS_{CERT,KEY}_PATH auto-derived from the mint above.
  const certEnv = certRes.exit === 0
    ? ` ${remoteEnvAssignment("ROOST_TLS_CERT_PATH", remoteCertPath)} ${remoteEnvAssignment("ROOST_TLS_KEY_PATH", remoteKeyPath)}`
    : "";
  // Stamp the local git HEAD into the remote install env so the deployed
  // worker reports it via heartbeat. .git isn't rsynced (apps/worker only),
  // so install.sh can't resolve HEAD on the remote — must pass it in.
  // Guard: refuse to deploy with uncommitted changes unless ROOST_ALLOW_DIRTY=1.
  // Otherwise rsync ships post-commit working-tree contents but the GIT_SHA
  // stamp captures the prior HEAD — drift badge fires falsely until the next
  // deploy after commit. Hit me once already (sb29 → sb30 keeper-bump deploy).
  const localGitSha = resolveLocalGitShaOrDie();
  const gitShaEnv = localGitSha ? ` ${remoteEnvAssignment("GIT_SHA", localGitSha)}` : "";
  const installEnv: Array<[string, string | undefined]> = [
    ["ROOST_COORDINATOR_URL", resolvedCoordinatorUrl],
    ["ROOST_BOOTSTRAP_TOKEN", resolved("ROOST_BOOTSTRAP_TOKEN")],
    ["ROOST_WORKER_LABEL", resolved("ROOST_WORKER_LABEL")],
    ["ROOST_REACHABLE_ADDR", resolvedReachableAddr],
  ];
  const passthroughEnv = installEnv
    .filter((entry): entry is [string, string] => entry[1] !== undefined)
    .map(([key, value]) => remoteEnvAssignment(key, value))
    .join(" ") + certEnv + gitShaEnv;
  const installSh = await sshExec(
    host,
    `${passthroughEnv} bash ${REMOTE_DIR}/apps/worker/scripts/install.sh install 2>&1`,
  );
  if (installSh.exit !== 0) {
    console.error("install.sh failed:");
    console.error(installSh.stdout);
    console.error(installSh.stderr);
    process.exit(5);
  }
  console.log(installSh.stdout.trim().split("\n").map((l) => `   ${l}`).join("\n"));

  console.log(`>> kickstart com.roost.worker-v2 on ${host}`);
  // install.sh already kickstarts, but bounce again after rsync to be sure.
  const kick = await sshExec(host, `launchctl kickstart -k gui/$(id -u)/com.roost.worker-v2 2>&1`);
  if (kick.exit !== 0) {
    console.error(`kickstart failed (exit ${kick.exit}):`);
    console.error(kick.stdout);
    console.error(kick.stderr);
    process.exit(4);
  }

  console.log(`>> verifying service is up on ${host}`);
  // Give launchd a beat then read the print output.
  const { promise: settled, resolve: markSettled } = Promise.withResolvers<void>();
  setTimeout(markSettled, 1500);
  await settled;
  const verify = await sshExec(host, verifyWorkerCmd("darwin"));
  finishWorkerDeploy(verify, `>> done — ${host} v2 worker deployed`);
}
