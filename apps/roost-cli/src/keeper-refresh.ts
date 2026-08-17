// `roost keeper-refresh <host> --yes` is the only workflow authorized to
// stop a keeper. It is destructive, explicitly confirmed, and serialized
// with update/coordinator-relocation through the machine transaction lock.
import { acquireMachineTransaction } from "@roost/shared/machine-transaction";
import { roostServiceDir } from "@roost/shared/paths";
import { mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { _isSelfHost, sshExec } from "./deploy.ts";

const KEEPER_PROC_PATTERN = "multiplexed-main.ts";

export async function keeperRefresh(args: string[]): Promise<void> {
  const host = args.find((argument) => !argument.startsWith("--"));
  if (!host) {
    console.error("usage: roost keeper-refresh <host> --yes");
    process.exit(2);
  }
  if (!args.includes("--yes")) {
    console.error(`Refreshing the keeper on ${host} will re-spawn every live session there,`);
    console.error("losing its scrollback and running subprocesses");
    console.error("(session ids + cwd survive). Re-run with --yes to proceed.");
    process.exit(1);
  }

  if (process.platform === "win32") {
    throw new Error(
      "Windows keeper-refresh is disabled outside RoostUpdaterV2; direct SCM mutation is not authorized",
    );
  }
  if (process.platform !== "darwin" && process.platform !== "linux") {
    throw new Error(`unsupported keeper-refresh platform: ${process.platform}`);
  }

  const journalPath = join(
    roostServiceDir(undefined, process.platform),
    "transactions",
    "keeper-refresh.json",
  );
  mkdirSync(dirname(journalPath), { recursive: true });
  const transaction = await acquireMachineTransaction("keeper-refresh", journalPath);
  try {
    const self = await _isSelfHost(host);

    const remoteCmd = `pkill -TERM -f ${KEEPER_PROC_PATTERN}`;
    if (self) {
      console.log(`>> local keeper-refresh (pkill -TERM -f ${KEEPER_PROC_PATTERN})`);
      const proc = Bun.spawn({
        cmd: ["pkill", "-TERM", "-f", KEEPER_PROC_PATTERN],
        stdio: ["inherit", "inherit", "inherit"],
      });
      await proc.exited;
      // pkill 0 = signalled; 1 = no match. Both retain the existing behavior.
      console.log(proc.exitCode === 0
        ? ">> keeper signalled; worker will re-spawn it on current code"
        : ">> no running keeper matched (a fresh one spawns on next use)");
      return;
    }

    console.log(`>> ssh ${host} '${remoteCmd}'`);
    const result = await sshExec(host, remoteCmd);
    if (result.exit === 0) {
      console.log(">> keeper signalled; worker will re-spawn it on current code");
    } else if (result.exit === 1) {
      console.log(">> no running keeper matched (a fresh one spawns on next use)");
    } else {
      throw new Error(`ssh pkill failed (exit ${result.exit}): ${result.stderr.trim()}`);
    }
  } finally {
    await transaction.release();
  }
}
