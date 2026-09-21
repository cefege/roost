// Internal read-only settlement proof for coordinator-owned worker update jobs.
// It uses each platform owner's journal path and lease implementation; absence
// of a local child PID or a worker heartbeat is never treated as settlement.

import { existsSync, mkdirSync, realpathSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { posixShellQuote } from "@roost/shared/shell-quote";
import { roostServiceDir } from "@roost/shared/paths";
import {
  acquireRemoteDeployLock,
  failDeploy,
  releaseRemoteDeployLock,
  remoteMachineTransactionPath,
  sshExec,
} from "./deploy-exec.ts";
import { _backfillEnvFromPlist } from "./deploy-plist-env.ts";
import { _isSelfHost } from "./deploy-self-host.ts";
import { localWorkerDeployJournalPath } from "./local-worker-deploy-journal.ts";
import { linuxDeployJournalPath } from "./linux-deploy-journal.ts";
import { _macosDeployJournalPath } from "./deploy-macos-journal.ts";
import { acquireMachineTransaction } from "./machine-transaction.ts";

export async function deploySettlementProbe(args: string[]): Promise<void> {
  const host = args[0];
  if (!host || args.length !== 1 || !/^[A-Za-z0-9.-]+$/.test(host)) {
    failDeploy(1, "internal deploy settlement probe requires one host");
  }
  if (await _isSelfHost(host)) {
    const serviceDir = resolve(roostServiceDir());
    mkdirSync(serviceDir, { recursive: true, mode: 0o700 });
    const journalPath = localWorkerDeployJournalPath(realpathSync(serviceDir));
    const transaction = await acquireMachineTransaction("deploy", journalPath);
    try {
      if (existsSync(journalPath)) failDeploy(5, "local worker deploy journal remains unsettled");
    } finally {
      await transaction.release();
    }
    return;
  }

  const platformResult = await sshExec(host, "uname -s");
  const platform = platformResult.stdout.trim();
  if (platformResult.exit !== 0 || (platform !== "Linux" && platform !== "Darwin")) {
    failDeploy(2, "worker platform is unavailable for journal settlement proof");
  }
  const { env } = await _backfillEnvFromPlist(host);
  const os = platform === "Linux" ? "linux" : "darwin";
  const lockPath = remoteMachineTransactionPath(os, env);
  const ownerId = `settlement-${crypto.randomUUID()}`;
  const lease = await acquireRemoteDeployLock(host, lockPath, ownerId);
  try {
    let journalPath: string;
    if (os === "linux") {
      const homeResult = await sshExec(host, "set -e; cd ~ && pwd", lease.signal);
      const home = homeResult.stdout.trim();
      if (homeResult.exit !== 0 || !home.startsWith("/")) {
        failDeploy(2, "worker home is unavailable for journal settlement proof");
      }
      journalPath = linuxDeployJournalPath(lockPath, home);
    } else {
      journalPath = _macosDeployJournalPath(lockPath);
    }
    const journal = posixShellQuote(journalPath);
    const proof = await sshExec(
      host,
      `test ! -e ${journal} && test ! -L ${journal}`,
      lease.signal,
    );
    if (proof.exit !== 0) failDeploy(5, `worker deploy journal remains unsettled: ${journalPath}`);
  } finally {
    await releaseRemoteDeployLock(host, lockPath, ownerId);
  }
}
