import { Database } from "bun:sqlite";
import { describe, expect, test } from "bun:test";
import { EventEmitter } from "node:events";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { _remoteDeployLockCommands, _startRemoteDeployLockRefreshForTest, DeployFailure, finishWorkerDeploy, REMOTE_MACHINE_TRANSACTION_PATHS, remoteMachineTransactionPath, run, workerServiceIsRunning, workerServiceMatchesRelease } from "../src/deploy-exec.ts";
import { _linuxTargetVerificationCommand } from "../src/linux-deploy-journal-commands.ts";
import type { LinuxDeployJournal } from "../src/linux-deploy-journal.ts";
import {
  linuxCoordinatorWorkingDirectoryCommand,
  linuxWorkerResourceEnvironment,
  shouldRemovePriorWorkerRelease,
} from "../src/deploy-linux.ts";
import { _activateLocalWorker } from "../src/deploy-local.ts";
import { localWorkerReleaseMatches } from "../src/local-worker-deploy-journal.ts";

describe("worker deployment verification", () => {
  test("a failed verification preserves output and never prints success", () => {
    const logs: string[] = [];
    const originalLog = console.log;
    console.log = (...args: unknown[]) => { logs.push(args.join(" ")); };
    try {
      expect(() => finishWorkerDeploy(
        { exit: 3, stdout: "service state: failed", stderr: "launchctl: not found" },
        ">> done — worker deployed",
        "darwin",
      )).toThrow(/service state: failed[\s\S]*launchctl: not found/);
    } finally {
      console.log = originalLog;
    }
  });

  test("a successful verification prints state before the completion marker", () => {
    const logs: string[] = [];
    const originalLog = console.log;
    console.log = (...args: unknown[]) => { logs.push(args.join(" ")); };
    try {
      finishWorkerDeploy(
        { exit: 0, stdout: "active count = 1\nstate = running\npid = 123\n", stderr: "" },
        ">> done — worker deployed",
        "darwin",
      );
      expect(logs).toEqual([
        "   active count = 1\n   state = running\n   pid = 123",
        ">> done — worker deployed",
      ]);
    } finally {
      console.log = originalLog;
    }
  });

  test("launchd spawn scheduling is not a successful deployment", () => {
    const output = "active count = 0\nstate = spawn scheduled\n";
    expect(workerServiceIsRunning(output, "darwin")).toBe(false);
    expect(() => finishWorkerDeploy(
      { exit: 0, stdout: output, stderr: "" },
      ">> done — worker deployed",
      "darwin",
    )).toThrow(/worker service verification failed/);
  });

  test("systemd requires active, running, and a non-zero PID", () => {
    expect(workerServiceIsRunning(
      "MainPID=420\nActiveState=active\nSubState=running\n",
      "linux",
    )).toBe(true);
    expect(workerServiceIsRunning(
      "MainPID=0\nActiveState=active\nSubState=exited\n",
      "linux",
    )).toBe(false);
  });

  test("deployment identity requires the activated release marker", () => {
    expect(workerServiceMatchesRelease("state = running\nRoostReleaseMatch=yes\n")).toBe(true);
    expect(workerServiceMatchesRelease("state = running\nRoostReleaseMatch=no\n")).toBe(false);
    expect(workerServiceMatchesRelease("state = running\n")).toBe(false);
  });

  test("remote deploy lock follows the installed worker data root", () => {
    expect(remoteMachineTransactionPath("linux", {
      ROOST_WORKER_DATA_DIR: "/srv/roost-worker",
    })).toBe("/srv/roost-worker/service/machine-transaction.sqlite");
    expect(remoteMachineTransactionPath("darwin", {
      ROOST_SERVICE_DIR: "/Volumes/Secure/Roost Service",
      ROOST_WORKER_DATA_DIR: "/ignored",
    })).toBe("/Volumes/Secure/Roost Service/machine-transaction.sqlite");
    expect(remoteMachineTransactionPath("darwin", {
      ROOST_SERVICE_DIR: "/Volumes/Secure/Roost Service ",
    })).toBe("/Volumes/Secure/Roost Service /machine-transaction.sqlite");
    expect(() => remoteMachineTransactionPath("linux", {
      ROOST_WORKER_DATA_DIR: "../relative",
    })).toThrow("installed machine transaction path is unsafe");
  });

  test("remote deploy lock serializes stale takeover and preserves owner-safe release", async () => {
    const home = mkdtempSync(join(tmpdir(), "roost-deploy-lock-"));
    expect(REMOTE_MACHINE_TRANSACTION_PATHS.linux).toBe(
      ".local/share/RoostWorkerV2/service/machine-transaction.sqlite",
    );
    expect(REMOTE_MACHINE_TRANSACTION_PATHS.darwin).toBe(
      "Library/Application Support/RoostWorkerV2/service/machine-transaction.sqlite",
    );
    const lockPath = REMOTE_MACHINE_TRANSACTION_PATHS.darwin;
    const databasePath = join(home, lockPath);
    const ownerA = _remoteDeployLockCommands(lockPath, "owner-a", 60);
    const ownerB = _remoteDeployLockCommands(lockPath, "owner-b", 60);
    const ownerC = _remoteDeployLockCommands(lockPath, "owner-c", 60);
    const env = { ...process.env, HOME: home };
    const run = (command: string) => Bun.spawnSync(["bash", "-lc", command], { env });
    const readLease = () => {
      const database = new Database(databasePath, { readonly: true });
      try {
        return database.query<{
          owner: string;
          created_at: number;
          expires_at: number;
        }, []>("SELECT owner, created_at, expires_at FROM deploy_lease WHERE singleton = 1").get();
      } finally {
        database.close();
      }
    };

    try {
      expect(run(ownerA.acquire).exitCode).toBe(0);
      expect(existsSync(databasePath)).toBe(true);
      expect(existsSync(`${databasePath}.sqlite`)).toBe(false);
      expect(readLease()).toMatchObject({
        owner: "owner-a",
        created_at: expect.any(Number),
        expires_at: expect.any(Number),
      });
      expect(run(ownerA.renew).exitCode).toBe(0);

      expect(run(ownerB.acquire).exitCode).toBe(75);
      expect(run(ownerB.renew).exitCode).toBe(74);
      expect(run(ownerB.release).exitCode).toBe(0);
      expect(readLease()?.owner).toBe("owner-a");

      const database = new Database(databasePath);
      try {
        database.query("UPDATE deploy_lease SET expires_at = 0 WHERE singleton = 1").run();
      } finally {
        database.close();
      }
      expect(run(ownerA.renew).exitCode).toBe(74);
      const contenders = [ownerB, ownerC].map(({ acquire }) => Bun.spawn({
        cmd: ["bash", "-lc", acquire],
        env,
        stdin: "ignore",
        stdout: "ignore",
        stderr: "ignore",
      }));
      const exits = await Promise.all(contenders.map((process) => process.exited));
      expect(exits.sort((a, b) => a - b)).toEqual([0, 75]);

      const winner = readLease()?.owner;
      expect(winner === "owner-b" || winner === "owner-c").toBe(true);
      expect(run(ownerA.release).exitCode).toBe(0);
      expect(readLease()?.owner).toBe(winner);
      expect(run(ownerB.release).exitCode).toBe(0);
      expect(run(ownerC.release).exitCode).toBe(0);
      expect(readLease()).toBeNull();

      const localTransaction = new Database(databasePath);
      try {
        localTransaction.exec(`
          CREATE TABLE IF NOT EXISTS active_machine_transaction (
            singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
            record_json TEXT NOT NULL
          )
        `);
        localTransaction.query(
          "INSERT INTO active_machine_transaction (singleton, record_json) VALUES (1, ?)",
        ).run(JSON.stringify({ kind: "update" }));
      } finally {
        localTransaction.close();
      }
      expect(run(ownerA.acquire).exitCode).toBe(75);
      expect(readLease()).toBeNull();

      const clearLocalTransaction = new Database(databasePath);
      try {
        clearLocalTransaction.query(
          "DELETE FROM active_machine_transaction WHERE singleton = 1",
        ).run();
      } finally {
        clearLocalTransaction.close();
      }
      expect(run(ownerA.acquire).exitCode).toBe(0);
      expect(run(ownerA.release).exitCode).toBe(0);
    } finally {
      rmSync(home, { recursive: true, force: true });
    }
  });

  for (const [signalName, exitCode] of [["SIGINT", 130], ["SIGTERM", 143]] as const) {
    test(`${signalName} aborts an in-flight lease command and removes handlers`, async () => {
      const signals = new EventEmitter();
      const renewalStarted = Promise.withResolvers<void>();
      let renewalSignal: AbortSignal | undefined;
      let renewalCalls = 0;
      const refresh = _startRemoteDeployLockRefreshForTest(
        async (signal) => {
          renewalCalls += 1;
          renewalSignal = signal;
          renewalStarted.resolve();
          const renewalDone = Promise.withResolvers<{ exit: number; stdout: string; stderr: string }>();
          const finish = () => renewalDone.resolve({ exit: 255, stdout: "", stderr: "ssh interrupted" });
          signal.addEventListener("abort", finish, { once: true });
          if (signal.aborted) finish();
          return await renewalDone.promise;
        },
        signals,
      );

      expect(signals.listenerCount("SIGINT")).toBe(1);
      expect(signals.listenerCount("SIGTERM")).toBe(1);
      const renewal = refresh.renewNow();
      await renewalStarted.promise;
      expect(signals.emit(signalName)).toBe(true);
      expect(renewalSignal?.aborted).toBe(true);
      expect(refresh.signal.aborted).toBe(true);
      expect(refresh.signal.reason).toBeInstanceOf(DeployFailure);
      expect(refresh.signal.reason).toMatchObject({
        exitCode,
        message: `deployment interrupted by ${signalName}`,
      });

      await renewal;
      await refresh.renewNow();
      expect(renewalCalls).toBe(1);
      const failure = await refresh.stop();
      expect(failure).toBe(refresh.signal.reason);
      expect(signals.listenerCount("SIGINT")).toBe(0);
      expect(signals.listenerCount("SIGTERM")).toBe(0);
    });
  }

  test("Linux success cleanup accepts an absent coordinator but rejects ambiguous paths", () => {
    const home = "/home/worker";
    const prior = `${home}/.local/share/roost/releases/worker/prior`;
    const current = `${home}/.local/share/roost/releases/worker/current`;
    expect(shouldRemovePriorWorkerRelease(prior, current, null, home)).toBe(true);
    expect(shouldRemovePriorWorkerRelease(prior, current, "", home)).toBe(false);
    expect(shouldRemovePriorWorkerRelease(prior, current, prior, home)).toBe(false);
    expect(shouldRemovePriorWorkerRelease(prior, current, "/srv/coord", home)).toBe(true);
    expect(shouldRemovePriorWorkerRelease("/srv/shared", current, "/srv/coord", home)).toBe(false);
  });

  test("Linux coordinator discovery supplies the non-login user-bus runtime", () => {
    const root = mkdtempSync(join(tmpdir(), "roost-coord-lookup-"));
    const systemctl = join(root, "systemctl");
    writeFileSync(
      systemctl,
      "#!/bin/sh\n[ \"$XDG_RUNTIME_DIR\" = \"/run/user/$(id -u)\" ] || exit 55\ncase \"$*\" in *LoadState*) printf 'loaded\\n';; *WorkingDirectory*) printf '/srv/coord\\n';; *) exit 56;; esac\n",
    );
    chmodSync(systemctl, 0o700);
    try {
      const result = Bun.spawnSync(["bash", "-c", linuxCoordinatorWorkingDirectoryCommand()], {
        env: {
          ...process.env,
          PATH: `${root}:${process.env.PATH ?? "/usr/bin:/bin"}`,
          XDG_RUNTIME_DIR: "",
        },
      });
      expect(result.exitCode).toBe(0);
      expect(result.stdout.toString().trim()).toBe("/srv/coord");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
  // skipIf linux: the proof reads /proc/<pid>/environ (procfs), honest only on the platform it ships to — systemd worker targets, covered by CI's ubuntu-latest job; elsewhere the generated bash exits 1 on a missing /proc.
  test.skipIf(process.platform !== "linux")("Linux target proof rejects pre-activation epoch", async () => {
    const home = mkdtempSync(join(tmpdir(), "roost-linux-proof-"));
    const target = join(home, ".local", "share", "roost", "releases", "worker", "target");
    const tools = join(home, "tools");
    mkdirSync(target, { recursive: true });
    mkdirSync(tools);
    const systemctl = join(tools, "systemctl");
    writeFileSync(
      systemctl,
      "#!/bin/sh\ncase \"$*\" in *ActiveState*) printf 'ActiveState=active\\nSubState=running\\nMainPID=%s\\n' \"$ROOST_TEST_PID\";; *MainPID*) printf '%s\\n' \"$ROOST_TEST_PID\";; *) exit 57;; esac\n",
    );
    chmodSync(systemctl, 0o700);
    const child = Bun.spawn(["bash", "-c", "exec sleep 30"], {
      cwd: target,
      env: { ...process.env, GIT_SHA: "a".repeat(40) },
      stdout: "ignore",
      stderr: "ignore",
    });
    await Bun.sleep(25);
    const base: LinuxDeployJournal = {
      phase: "activating",
      targetSha: "a".repeat(40),
      targetReleasePath: target,
      priorUnit: "[Service]\n",
      priorUnitMode: 0o600,
      priorLifecycle: "running",
      priorEnablement: "enabled",
      priorPid: child.pid,
    };
    const runProof = (priorPid: number) => Bun.spawnSync(
      ["bash", "-c", _linuxTargetVerificationCommand({ ...base, priorPid }, home)],
      {
        env: {
          ...process.env,
          PATH: `${tools}:${process.env.PATH ?? "/usr/bin:/bin"}`,
          ROOST_TEST_PID: String(child.pid),
          XDG_RUNTIME_DIR: "",
        },
      },
    );
    try {
      expect(runProof(child.pid).exitCode).not.toBe(0);
      const advanced = runProof(child.pid + 1);
      expect(advanced.exitCode, advanced.stderr.toString()).toBe(0);
      expect(advanced.stdout.toString()).toContain("RoostReleaseMatch=yes");
    } finally {
      child.kill();
      await child.exited;
      rmSync(home, { recursive: true, force: true });
    }
  });
  test("staged Linux workers carry effective resource and logrotate settings", () => {
    const definition = [
      "[Service]",
      'Environment="ROOST_WORKER_LOGROTATE_CONF=/srv/worker%%logs/worker.conf"',
      "MemoryHigh=12G",
      "TasksMax=8192",
    ].join("\n");
    expect(linuxWorkerResourceEnvironment(definition)).toEqual({
      ROOST_WORKER_MEMORY_HIGH: "12G",
      ROOST_WORKER_TASKS_MAX: "8192",
      ROOST_WORKER_LOGROTATE_CONF: "/srv/worker%logs/worker.conf",
    });
  });

  test("aborted deployment signals refuse new processes and stop active ones", async () => {
    const preAborted = new AbortController();
    preAborted.abort(new Error("lease lost"));
    const marker = join(tmpdir(), `roost-abort-${crypto.randomUUID()}`);
    const refused = await run(["bash", "-c", `touch '${marker}'`], {
      quiet: true,
      signal: preAborted.signal,
    });
    expect(refused.exit).toBe(9);
    expect(existsSync(marker)).toBe(false);

    const active = new AbortController();
    const running = run(["bash", "-c", "while :; do :; done"], {
      quiet: true,
      signal: active.signal,
    });
    await Bun.sleep(20);
    active.abort(new Error("lease lost"));
    expect((await running).exit).not.toBe(0);
  });

  test("local activation rolls back and removes its stage when release proof fails", async () => {
    let rollbacks = 0;
    let cleanups = 0;
    const running = {
      exit: 0,
      stdout: "MainPID=42\nActiveState=active\nSubState=running\n",
      stderr: "",
    };
    await expect(_activateLocalWorker({
      install: async () => ({ exit: 0, stdout: "installed", stderr: "" }),
      restart: async () => ({ exit: 0, stdout: "", stderr: "" }),
      verify: async () => running,
      rollback: async () => {
        rollbacks += 1;
        return null;
      },
      cleanupStage: async () => {
        cleanups += 1;
      },
    })).rejects.toThrow("prior worker service restored");
    expect(rollbacks).toBe(1);
    expect(cleanups).toBe(1);
  });

  test("local release proof binds both worktree and git identity", () => {
    const definition = [
      "[Service]",
      'WorkingDirectory="/srv/releases/worker/sha-1"',
      'Environment="GIT_SHA=sha-1"',
    ].join("\n");
    expect(localWorkerReleaseMatches(
      definition,
      "linux",
      "/srv/releases/worker/sha-1",
      "sha-1",
    )).toBe(true);
    expect(localWorkerReleaseMatches(
      definition,
      "linux",
      "/srv/releases/worker/sha-2",
      "sha-1",
    )).toBe(false);
  });
});
