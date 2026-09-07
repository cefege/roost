// `roost keeper-refresh <host> --yes` performs coordinator-fenced empty-only
// maintenance; `--force-live` is the operator's break-glass that additionally
// destroys live PTYs. The machine transaction serializes deploys while the
// coordinator drains every channel-creating command and authorizes the worker.

import { mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { roostServiceDir } from "@roost/shared/paths";
import {
  acquireRemoteDeployLock,
  releaseRemoteDeployLock,
  remoteMachineTransactionPath,
  sshExec,
} from "./deploy-exec.ts";
import { _isSelfHost } from "./deploy-self-host.ts";
import { _backfillEnvFromPlist } from "./deploy-plist-env.ts";
import {
  localUpdateWorker,
  prepareKeeperMaintenance,
  workerForDirectKeeperTarget,
} from "./direct-keeper-update.ts";
import { acquireMachineTransaction } from "./machine-transaction.ts";
import type { WorkerStatus } from "./status.ts";

function keeperRefreshJournalPath(platform: "darwin" | "linux"): string {
  return join(
    roostServiceDir(undefined, platform),
    "transactions",
    "keeper-refresh.json",
  );
}

function reportForcedDestruction(host: string, worker: WorkerStatus): void {
  const keeper = worker.keeperRuntime!;
  console.error(`--force-live will DESTROY every PTY the keeper on ${host} hosts.`);
  console.error(`  keeper pid ${keeper.keeper_pid}, epoch ${keeper.keeper_epoch}`);
  console.error(`  keeper channels: ${keeper.channel_count}`);
  console.error(
    `  coordinator sessions: ${worker.coordinatorOpenSessionIds.length}`
    + (worker.coordinatorOpenSessionIds.length === 0
      ? ""
      : ` (${worker.coordinatorOpenSessionIds.join(", ")})`),
  );
  console.error("  Every shell, dev server, and test running in them exits.");
}

export async function keeperRefresh(args: string[]): Promise<void> {
  const localPlatform = process.platform;
  const host = args.find(argument => !argument.startsWith("--"));
  const forceLive = args.includes("--force-live");
  if (!host) {
    console.error("usage: roost keeper-refresh <host> --yes [--force-live]");
    process.exit(2);
  }
  if (!args.includes("--yes")) {
    console.error(forceLive
      ? `Destroying the live keeper on ${host} requires confirmation.`
      : `Refreshing the empty keeper on ${host} requires confirmation.`);
    console.error(forceLive
      ? "--force-live ends every PTY the keeper hosts. Re-run with --yes."
      : "Keepers with live channels are refused without --force-live. Re-run with --yes.");
    process.exit(1);
  }
  if (localPlatform === "win32") {
    throw new Error(
      "Windows keeper-refresh is disabled outside RoostUpdaterV2; direct SCM mutation is not authorized",
    );
  }
  if (localPlatform !== "darwin" && localPlatform !== "linux") {
    throw new Error(`unsupported keeper-refresh platform: ${localPlatform}`);
  }
  const selfHost = await _isSelfHost(host);
  const worker = selfHost
    ? await localUpdateWorker()
    : workerForDirectKeeperTarget(host);
  if (worker.stale) throw new Error(`${worker.label}: keeper runtime proof is stale`);
  // Destroying PTYs requires a proven keeper identity. Without the coordinator's
  // runtime proof there is nothing to authorize the destruction of.
  if (forceLive && !worker.keeperRuntime) {
    throw new Error(
      `${worker.label}: keeper identity is unproven; --force-live is refused`,
    );
  }
  if (forceLive) reportForcedDestruction(host, worker);

  let release: () => Promise<void>;
  if (selfHost) {
    const journalPath = keeperRefreshJournalPath(localPlatform);
    mkdirSync(dirname(journalPath), { recursive: true });
    const transaction = await acquireMachineTransaction("keeper-refresh", journalPath);
    release = () => transaction.release();
  } else {
    const platformProbe = await sshExec(host, "uname -s");
    const platform = platformProbe.stdout.trim();
    if (platformProbe.exit !== 0 || (platform !== "Darwin" && platform !== "Linux")) {
      throw new Error(
        `cannot identify keeper-refresh target platform: ${platformProbe.stderr.trim()}`,
      );
    }
    const { env } = await _backfillEnvFromPlist(host);
    const deployPlatform = platform === "Linux" ? "linux" : "darwin";
    const lockPath = remoteMachineTransactionPath(deployPlatform, env);
    const lockOwner = `keeper-refresh-${crypto.randomUUID()}`;
    await acquireRemoteDeployLock(host, lockPath, lockOwner);
    release = () => releaseRemoteDeployLock(host, lockPath, lockOwner);
  }
  try {
    const outcome = await prepareKeeperMaintenance(worker.fingerprint, forceLive);
    console.log(JSON.stringify({ outcome }));
  } finally {
    await release();
  }
}
