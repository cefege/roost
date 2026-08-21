// Guards the "closing a pane leaks the child process tree" fix
// (plan yeah-i-mean-please-harmonic-wilkes / CLAUDE.md kill-reap row).
//
// Pre-fix, the keeper's KillChild handler did a single
// `ch.proc.kill("SIGTERM")` (multiplexed-main.ts:322) — SIGTERM to the ONE
// session-leader pid. That leaks two ways this test exercises:
//   1. Shell panes spawn an INTERACTIVE shell, which sets SIGTERM->SIG_IGN
//      → the kill is a no-op, the shell never dies, the tty hangup cascade
//      never fires.
//   2. A FOREGROUND job (`sleep 99…`) runs in its own job-control process
//      group; a BACKGROUND job (`sleep 88… &`) in yet another. A single-pid
//      SIGTERM reaches none of them.
// Post-fix reapChannelTree does: pre-death `ps` tree snapshot → close the PTY
// master (SIGHUP → foreground group) + group SIGTERM → 2s grace → SIGKILL
// every survivor in the snapshot. All three pids die.
//
// Real keeper subprocess + real PTYs + real `sleep` children + real `pgrep`/
// `kill` — no mocks (feedback_no_mock_claude_use_real). Liveness oracle is
// `kill(pid,0)`→ESRCH, same as keeper-death-reconcile.test.ts.

import { describe, test, expect, afterAll } from "bun:test";
import { rmSync } from "node:fs";
import { join } from "node:path";
import { homedir, tmpdir } from "node:os";
import { MultiplexedKeeperPool, type MuxChannelCallbacks } from "../src/keeper/multiplexed-client.ts";
import { keeperTestShellSpec } from "./keeper-test-fixtures.ts";

const SOCK_DIR = join(tmpdir(), `roost-test-keeper-reap-${process.pid}`);
process.env.ROOST_WORKER_DATA_DIR = SOCK_DIR;
process.env.ROOST_KEEPER_QUIET = "1";
const SOCK_PATH = join(SOCK_DIR, "mux-keeper.sock");

// Distinctive sleep durations so pgrep -f can't match a daily-driver process
// or another test file. ~10-day sleeps; reaped in afterAll regardless.
const FG_MARK = "sleep 8873310";   // foreground job (own pgid, tty foreground)
const BG_MARK = "sleep 9982210";   // background job (own pgid, backgrounded via &)
const HUP_MARK = "sleep 7761100";  // nohup'd job — ignores SIGHUP, only SIGKILL reaps it

const pool = new MultiplexedKeeperPool();
let _nextCh = 900;
const spawnedPids: number[] = [];

afterAll(() => {
  // Reap anything a RED run left alive so the test never leaks its own sleeps.
  for (const pid of spawnedPids) try { process.kill(pid, "SIGKILL"); } catch { /* gone */ }
  for (const mark of [FG_MARK, BG_MARK, HUP_MARK]) for (const pid of pgrepF(mark)) {
    try { process.kill(pid, "SIGKILL"); } catch { /* gone */ }
  }
  // dispose() leaves the keeper running by design, so reap it by the handle we
  // hold — captured before dispose() drops the socket.
  const keeperPid = pool._keeperProc?.pid;
  pool.dispose();
  if (keeperPid) try { process.kill(keeperPid, "SIGKILL"); } catch { /* already dead */ }
  for (const pid of pgrepF(SOCK_PATH)) try { process.kill(pid, "SIGKILL"); } catch { /* gone */ }
  // Whole dir: unlinking only the socket left SOCK_DIR in $TMPDIR every run.
  rmSync(SOCK_DIR, { recursive: true, force: true });
});

/** pgrep -f <pattern> → array of matching pids (pgrep excludes its own pid). */
function pgrepF(pattern: string): number[] {
  const out = Bun.spawnSync(["pgrep", "-f", pattern]).stdout.toString().trim();
  if (!out) return [];
  return out.split("\n").map((l) => Number(l.trim())).filter((n) => Number.isInteger(n) && n > 1);
}

function isAlive(pid: number): boolean {
  if (pid <= 1) return false;
  try { process.kill(pid, 0); return true; } catch { return false; }
}

async function waitFor(predicate: () => boolean, deadlineMs: number, stepMs = 50): Promise<boolean> {
  const deadline = Date.now() + deadlineMs;
  while (Date.now() < deadline) {
    if (predicate()) return true;
    await Bun.sleep(stepMs);
  }
  return predicate();
}

const noopCallbacks: MuxChannelCallbacks = {
  onOutput: () => { /* ground truth is pgrep/kill-0, not the byte stream */ },
  onExit: () => { /* shell won't exit pre-fix; post-fix it does — not asserted here */ },
  onError: () => { /* keeper lifecycle not under test */ },
};

describe("keeper-child-reap — closing a channel kills the whole process tree", () => {
  test("PTY owns a foreground process group and resize delivers SIGWINCH", async () => {
    const channelId = _nextCh++;
    let output = "";
    const shellPid = await pool.spawn({
      channelId,
      shellSpec: keeperTestShellSpec({
        executable: "/bin/bash",
        argv: ["--norc", "-i"],
        cwd: homedir(),
      }),
      cols: 80,
      rows: 24,
      callbacks: {
        onOutput: (bytes) => { output += Buffer.from(bytes).toString("utf8"); },
        onExit: () => { /* liveness is checked with kill(pid, 0) */ },
        onError: () => { /* assertions below expose keeper failures */ },
      },
    });

    pool.input(channelId, new TextEncoder().encode(
      "trap 'read r c < <(stty size); echo WINCH:$r:$c' WINCH; echo TRAP_READY\n",
    ));
    expect(await waitFor(() => output.includes("TRAP_READY"), 4000)).toBe(true);
    expect(output).not.toContain("no job control in this shell");

    const resize = pool.beginResize(channelId, 1, 70, 20);
    expect(resize.admission.written).toBe(true);
    expect(await resize.result).toEqual({ kind: "ack", seq: 1, cols: 70, rows: 20 });
    expect(await waitFor(() => output.includes("WINCH:20:70"), 4000)).toBe(true);

    pool.kill(channelId);
    expect(await waitFor(() => !isAlive(shellPid), 6000)).toBe(true);
  }, 30_000);

  test("interactive shell + foreground job + background job are ALL dead after kill", async () => {
    const channelId = _nextCh++;
    // Interactive bash: --norc for determinism, -i forces interactive so it
    // installs SIGTERM->SIG_IGN (the load-bearing pre-fix leak). Spawned on a
    // PTY by the keeper. spawn() resolves to the shell pid.
    const shellPid = await pool.spawn({
      channelId,
      shellSpec: keeperTestShellSpec({
        executable: "/bin/bash",
        argv: ["--norc", "-i"],
        cwd: homedir(),
      }),
      cols: 200, rows: 50,
      callbacks: noopCallbacks,
    });
    spawnedPids.push(shellPid);
    expect(shellPid).toBeGreaterThan(1);

    // Let bash reach its prompt, then launch a backgrounded + a foreground job.
    await Bun.sleep(700);
    pool.input(channelId, new TextEncoder().encode(`${BG_MARK} & ${FG_MARK}\n`));

    // Both jobs must be running before we kill.
    const started = await waitFor(() => pgrepF(FG_MARK).length > 0 && pgrepF(BG_MARK).length > 0, 4000);
    expect(started).toBe(true);
    const fgPid = pgrepF(FG_MARK)[0];
    const bgPid = pgrepF(BG_MARK)[0];
    spawnedPids.push(fgPid, bgPid);
    expect(isAlive(shellPid)).toBe(true);
    expect(isAlive(fgPid)).toBe(true);
    expect(isAlive(bgPid)).toBe(true);

    // Close the pane.
    pool.kill(channelId);

    // Within grace (2s) + margin, the shell, the foreground job, AND the
    // backgrounded job must all be gone. Pre-fix: shell ignores SIGTERM and
    // both jobs live in unsignaled process groups → all three survive → RED.
    const allDead = await waitFor(
      () => !isAlive(shellPid) && pgrepF(FG_MARK).length === 0 && pgrepF(BG_MARK).length === 0,
      6000,
    );
    if (!allDead) {
      throw new Error(
        `leaked after kill: shell=${isAlive(shellPid)} fg=${pgrepF(FG_MARK).length} bg=${pgrepF(BG_MARK).length}`,
      );
    }
    expect(allDead).toBe(true);
  }, 30_000);

  test("a nohup'd job that IGNORES SIGHUP is still reaped (escalation SIGKILL path)", async () => {
    const channelId = _nextCh++;
    const shellPid = await pool.spawn({
      channelId,
      shellSpec: keeperTestShellSpec({
        executable: "/bin/bash",
        argv: ["--norc", "-i"],
        cwd: homedir(),
      }),
      cols: 200, rows: 50,
      callbacks: noopCallbacks,
    });
    spawnedPids.push(shellPid);

    await Bun.sleep(700);
    // nohup sets SIGHUP->SIG_IGN then execs → master-close SIGHUP is a no-op on
    // it; the ONLY thing that kills it is the pre-death snapshot SIGKILL sweep.
    pool.input(channelId, new TextEncoder().encode(`nohup ${HUP_MARK} >/dev/null 2>&1 &\n`));

    const started = await waitFor(() => pgrepF(HUP_MARK).length > 0, 4000);
    expect(started).toBe(true);
    const hupPid = pgrepF(HUP_MARK)[0];
    spawnedPids.push(hupPid);
    expect(isAlive(hupPid)).toBe(true);

    pool.kill(channelId);

    // Must die within grace (2s) + margin — proves the SIGKILL escalation, not
    // just the SIGHUP, does the work for signal-ignoring processes.
    const dead = await waitFor(() => pgrepF(HUP_MARK).length === 0, 6000);
    if (!dead) throw new Error(`nohup'd job survived kill: ${pgrepF(HUP_MARK)}`);
    expect(dead).toBe(true);
  }, 30_000);
});
