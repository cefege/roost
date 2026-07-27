// `roost keeper-refresh <host> [--yes]` — deliberately replace a host's keeper
// so it re-spawns on the current code (the keeper outlives a deploy, so a
// behavior-only change stays dormant until the keeper restarts — surfaced as a
// "keeper stale" badge in the SPA MachinesPane).
//
// DESTRUCTIVE: SIGTERM to the keeper reaps every PTY child (reap fix) — the
// worker's setOnKeeperDeath then re-spawns sessions under the same ids, but
// they lose scrollback and running subprocesses. Hence the --yes gate.
//
// Mechanism: SIGTERM the keeper process (`pkill -f multiplexed-main.ts`), self
// locally or remote via ssh (reuses deploy.ts helpers). No coord RPC.

import { _isSelfHost, sshExec } from "./deploy.ts";

const KEEPER_PROC_PATTERN = "multiplexed-main.ts";

export async function keeperRefresh(args: string[]): Promise<void> {
  const host = args.find((a) => !a.startsWith("--"));
  if (!host) {
    console.error("usage: roost keeper-refresh <host> [--yes]");
    process.exit(2);
  }
  if (!args.includes("--yes")) {
    console.error(`Refreshing the keeper on ${host} will re-spawn every live session there,`);
    console.error("losing its scrollback and running subprocesses");
    console.error("(session ids + cwd survive). Re-run with --yes to proceed.");
    process.exit(1);
  }

  const remoteCmd = `pkill -TERM -f ${KEEPER_PROC_PATTERN}`;
  if (await _isSelfHost(host)) {
    console.log(`>> local keeper-refresh (pkill -TERM -f ${KEEPER_PROC_PATTERN})`);
    const proc = Bun.spawn({ cmd: ["pkill", "-TERM", "-f", KEEPER_PROC_PATTERN], stdio: ["inherit", "inherit", "inherit"] });
    await proc.exited;
    // pkill: 0 = matched+signalled, 1 = no match (keeper wasn't running → the
    // next worker op spawns a fresh one anyway). Either is success here.
    console.log(proc.exitCode === 0 ? ">> keeper signalled; worker will re-spawn it on current code" : ">> no running keeper matched (a fresh one spawns on next use)");
    return;
  }

  console.log(`>> ssh ${host} '${remoteCmd}'`);
  const r = await sshExec(host, remoteCmd);
  if (r.exit === 0) {
    console.log(">> keeper signalled; worker will re-spawn it on current code");
  } else if (r.exit === 1) {
    console.log(">> no running keeper matched (a fresh one spawns on next use)");
  } else {
    console.error(`ssh pkill failed (exit ${r.exit}): ${r.stderr.trim()}`);
    process.exit(r.exit);
  }
}
