// Pins the roll-forward terminal outcome shared by the macOS and Linux deploy
// journals: a rollback whose prior release provably cannot run clears its
// journal and keeps the staged release, while an un-started or unmigrated
// rollback still rolls back and keeps the journal. Also pins that journals
// written by the previous schema still parse and recover unchanged.

import { afterEach, describe, expect, spyOn, test } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DeployFailure } from "../src/deploy-exec.ts";
import { _parseMacosDeployJournal } from "../src/deploy-macos-journal.ts";
import {
  createMacosDeployJournalController,
  type MacosDeployJournalController,
} from "../src/deploy-macos-journal-controller.ts";
import { _recoverMacosDeployJournal } from "../src/deploy-macos-recovery.ts";
import { MACOS_DEPLOY_JOURNAL_PROGRAM } from "../src/macos-deploy-journal-program.ts";
import { _recoverLinuxDeployJournal } from "../src/deploy-linux-recovery.ts";
import { proveLinuxPriorService } from "../src/linux-prior-service-recovery.ts";
import { parseLinuxDeployJournalSnapshot } from "../src/linux-deploy-journal.ts";
import {
  DURABLE_WORKER_STATE_PROBE_JS,
  DURABLE_WORKER_STATE_PROBE_SH,
  DurableStateRollForwardRequired,
} from "../src/durable-worker-state.ts";
import {
  RELEASE_PATH,
  SHA,
  fakeRemote,
  journal,
} from "./deploy-macos-recovery-fixture.ts";
import {
  HOME,
  fakeRemote as fakeLinuxRemote,
  journalSnapshot,
} from "./deploy-linux-recovery-fixture.ts";

const MIGRATED_FORWARD = { priorDurableStateVersion: 1, targetDurableStateVersion: 2 } as const;
const UNMIGRATED = { priorDurableStateVersion: 2, targetDurableStateVersion: 2 } as const;
const RUNNING_PRIOR = { priorLifecycle: "running", priorPid: 41 } as const;
/** The exact launchd shape the fleet reported for a prior release gone dark. */
const LAUNCHD_MISSING = [
  `Could not find service "com.roost.worker-v2" in domain for user gui: 501`,
  "RoostLaunchdLoaded=no",
  "RoostLaunchdDisabled=no",
].join("\n");
const LINUX_JOURNAL_PATH = `${HOME}/.local/share/RoostWorkerV2/service/worker-deploy-journal`;
const LINUX_UNIT_PATH = `${HOME}/.config/systemd/user/roost-worker.service`;

const temporaryRoots: string[] = [];
afterEach(() => {
  for (const root of temporaryRoots.splice(0)) rmSync(root, { recursive: true, force: true });
});

function priorProofController(): MacosDeployJournalController {
  const home = mkdtempSync(join(tmpdir(), "roost-macos-prior-proof-"));
  temporaryRoots.push(home);
  return createMacosDeployJournalController(async (command) => {
    if (command.includes("bun -e")) {
      return { exit: 0, stdout: "RoostPriorDefinitionMatch=yes\n", stderr: "" };
    }
    return { exit: 1, stdout: LAUNCHD_MISSING, stderr: "" };
  }, join(home, "macos-worker-deploy-v1.json"));
}

interface RemoteProgramRun {
  action: string;
  journalPath: string;
  releaseRoot: string;
  durableStatePath: string;
}

function runRemoteProgram(run: RemoteProgramRun): {
  exitCode: number;
  stdout: string;
  stderr: string;
} {
  const result = Bun.spawnSync([process.execPath, "-e", MACOS_DEPLOY_JOURNAL_PROGRAM], {
    env: {
      ...process.env,
      ROOST_MAC_DEPLOY_ACTION: run.action,
      ROOST_MAC_DEPLOY_JOURNAL: run.journalPath,
      ROOST_MAC_DEPLOY_RELEASE_ROOT: run.releaseRoot,
      ROOST_MAC_DEPLOY_PLIST: join(run.releaseRoot, "..", "worker.plist"),
      ROOST_MAC_DEPLOY_DURABLE_STATE: run.durableStatePath,
      ROOST_MAC_DEPLOY_LABEL: "com.roost.worker-v2",
      ROOST_MAC_DEPLOY_ROLLOUT_ID: "",
      ROOST_MAC_DEPLOY_WORKER_FINGERPRINT: "",
      ROOST_MAC_DEPLOY_KEEPER_UPDATE: Buffer.from("null").toString("base64"),
    },
    stdout: "pipe",
    stderr: "pipe",
  });
  const decoder = new TextDecoder();
  return {
    exitCode: result.exitCode ?? 1,
    stdout: decoder.decode(result.stdout),
    stderr: decoder.decode(result.stderr),
  };
}

function loadWithRemoteProgram(value: unknown, releaseRoot: string): {
  exitCode: number;
  journal: { schemaVersion: number };
} {
  const home = mkdtempSync(join(tmpdir(), "roost-macos-journal-load-"));
  temporaryRoots.push(home);
  const journalPath = join(home, "journal.json");
  writeFileSync(journalPath, `${JSON.stringify(value)}\n`);
  const result = runRemoteProgram({
    action: "load",
    journalPath,
    releaseRoot,
    durableStatePath: join(home, "session-event-outbox.sqlite"),
  });
  const encoded = result.stdout.trim().slice("RoostMacDeployJournal=".length);
  return {
    exitCode: result.exitCode,
    journal: JSON.parse(Buffer.from(encoded, "base64").toString("utf8")).journal,
  };
}

describe("deploy journal roll-forward", () => {
  test("a retained macOS rollback whose prior release cannot run rolls forward", async () => {
    const durable = journal({
      phase: "rolling-back",
      keeperUpdate: null,
      workerFingerprint: null,
      ...RUNNING_PRIOR,
      ...MIGRATED_FORWARD,
    });
    const fixture = fakeRemote(
      durable,
      undefined,
      new DurableStateRollForwardRequired("macOS", 1, 2),
    );
    const notices: string[] = [];
    const errors = spyOn(console, "error").mockImplementation((line: unknown) => {
      notices.push(String(line));
    });
    try {
      await expect(_recoverMacosDeployJournal(fixture.remote)).resolves.toMatchObject({
        outcome: "roll-forward-required",
      });
    } finally {
      errors.mockRestore();
    }
    // The prior release is started before it is judged, the staged release
    // survives, and the journal that wedged every later deploy is gone.
    expect(fixture.calls).toEqual([
      "load", "bootout", "restore-prior:bytes", "disabled:false", "bootstrap", "kickstart",
      "disabled:false", "prove-prior:started", "clear",
    ]);
    expect(fixture.calls).not.toContain("remove-target");
    expect(notices.join("\n")).toContain("rollback is impossible");
    expect(notices.join("\n")).toContain("from schema 1 to 2");
    expect(notices.join("\n")).toContain(RELEASE_PATH);
  });

  test("an ordinary prior-proof failure still keeps the macOS journal", async () => {
    const fixture = fakeRemote(
      journal({ phase: "rolling-back", ...MIGRATED_FORWARD }),
      undefined,
      new DeployFailure(5, "prior macOS worker lifecycle did not round-trip; journal retained"),
    );
    await expect(_recoverMacosDeployJournal(fixture.remote))
      .rejects.toThrow("did not round-trip");
    expect(fixture.calls).not.toContain("clear");
  });

  test("an unloaded prior release is never started and still rolls back", async () => {
    const fixture = fakeRemote(journal({
      phase: "rolling-back",
      keeperUpdate: null,
      workerFingerprint: null,
      ...MIGRATED_FORWARD,
    }));
    await expect(_recoverMacosDeployJournal(fixture.remote)).resolves.toMatchObject({
      outcome: "rolled-back",
    });
    expect(fixture.calls).toEqual([
      "load", "bootout", "restore-prior:bytes", "bootout", "disabled:false",
      "prove-prior:unstarted", "remove-target", "clear",
    ]);
  });

  test("only a started prior release with a migrated store is unrecoverable", async () => {
    const sleeps = spyOn(Bun, "sleep").mockResolvedValue(undefined);
    try {
      const migrated = journal({ phase: "rolling-back", ...RUNNING_PRIOR, ...MIGRATED_FORWARD });
      await expect(priorProofController().recovery.provePrior(migrated, true))
        .rejects.toThrow(DurableStateRollForwardRequired);

      // Un-attempted: nothing was started, so nothing is proven unrunnable.
      await expect(priorProofController().recovery.provePrior(migrated, false))
        .rejects.toThrow("prior macOS worker lifecycle did not round-trip");

      // Started, but the store never moved: a transient failure keeps the journal.
      const unmigrated = journal({ phase: "rolling-back", ...RUNNING_PRIOR, ...UNMIGRATED });
      await expect(priorProofController().recovery.provePrior(unmigrated, true))
        .rejects.toThrow("prior macOS worker lifecycle did not round-trip");

      // A journal from the previous schema has no baseline to compare.
      const legacy = journal({ phase: "rolling-back", ...RUNNING_PRIOR });
      await expect(priorProofController().recovery.provePrior(legacy, true))
        .rejects.toThrow("prior macOS worker lifecycle did not round-trip");
    } finally {
      sleeps.mockRestore();
    }
  });

  test("a macOS journal written by the old schema still parses on both sides", () => {
    const releaseRoot = mkdtempSync(join(tmpdir(), "roost-macos-legacy-release-"));
    temporaryRoots.push(releaseRoot);
    const targetReleasePath = join(releaseRoot, `${SHA}-00000000-0000-4000-8000-000000000001`);
    mkdirSync(targetReleasePath);
    const legacy: Record<string, unknown> = {
      ...journal({ phase: "rolling-back", targetReleasePath, ...RUNNING_PRIOR }),
      schemaVersion: 2,
    };
    delete legacy.priorDurableStateVersion;
    delete legacy.targetDurableStateVersion;

    const local = _parseMacosDeployJournal(legacy, releaseRoot);
    expect(local.schemaVersion).toBe(3);
    expect(local.priorDurableStateVersion).toBeNull();
    expect(local.targetDurableStateVersion).toBeNull();
    const remote = loadWithRemoteProgram(legacy, releaseRoot);
    expect(remote.exitCode).toBe(0);
    expect(JSON.stringify(remote.journal)).toBe(JSON.stringify(local));
  });

  test("the macOS rollback checkpoint records the version the target left behind", () => {
    const home = mkdtempSync(join(tmpdir(), "roost-macos-rollback-checkpoint-"));
    temporaryRoots.push(home);
    const releaseRoot = join(home, "RoostWorkerV2-releases");
    const targetReleasePath = join(releaseRoot, `${SHA}-00000000-0000-4000-8000-000000000001`);
    mkdirSync(targetReleasePath, { recursive: true });
    const journalPath = join(home, "macos-worker-deploy-v1.json");
    const durableStatePath = join(home, "session-event-outbox.sqlite");
    const database = new Database(durableStatePath, { create: true });
    database.exec("CREATE TABLE session_events(client_seq INTEGER); PRAGMA user_version=2");
    database.close();
    writeFileSync(journalPath, `${JSON.stringify(journal({
      phase: "activated",
      targetReleasePath,
      keeperUpdate: null,
      workerFingerprint: null,
      priorDurableStateVersion: 1,
    }))}\n`);

    const checkpoint = runRemoteProgram({
      action: "checkpoint-rollback",
      journalPath,
      releaseRoot,
      durableStatePath,
    });
    expect(checkpoint.stderr).toBe("");
    expect(checkpoint.exitCode).toBe(0);
    const persisted = JSON.parse(readFileSync(journalPath, "utf8"));
    expect(persisted.phase).toBe("rolling-back");
    expect(persisted.priorDurableStateVersion).toBe(1);
    expect(persisted.targetDurableStateVersion).toBe(2);
  });

  test.skipIf(process.platform === "win32")(
    "both remote probes read the same durable store schema version",
    () => {
      const home = mkdtempSync(join(tmpdir(), "roost-durable-state-"));
      temporaryRoots.push(home);
      const store = join(home, "session-event-outbox.sqlite");
      const database = new Database(store, { create: true });
      database.exec("CREATE TABLE session_events(client_seq INTEGER); PRAGMA user_version=7");
      database.close();
      const decoder = new TextDecoder();
      const shell = Bun.spawnSync(
        ["sh", "-c", `${DURABLE_WORKER_STATE_PROBE_SH} durable_worker_state "$1"`, "sh", store],
        { stdout: "pipe", stderr: "pipe" },
      );
      expect(decoder.decode(shell.stdout)).toBe("7");
      const program = Bun.spawnSync([
        process.execPath,
        "-e",
        `const fs = await import("node:fs");\n`
          + `const durableStatePath = ${JSON.stringify(store)};\n`
          + `${DURABLE_WORKER_STATE_PROBE_JS}\nconsole.log(durableWorkerStateVersion());`,
      ], { stdout: "pipe", stderr: "pipe" });
      expect(decoder.decode(program.stdout).trim()).toBe("7");

      // An absent or unreadable store records nothing, so it can never make a
      // rollback look impossible.
      const absent = Bun.spawnSync(
        ["sh", "-c", `${DURABLE_WORKER_STATE_PROBE_SH} durable_worker_state "$1"`, "sh", join(home, "gone.sqlite")],
        { stdout: "pipe", stderr: "pipe" },
      );
      expect(decoder.decode(absent.stdout)).toBe("");
      expect(absent.exitCode).toBe(0);
    },
  );

  test("a retained Linux rollback whose prior unit cannot run rolls forward", async () => {
    const durable = parseLinuxDeployJournalSnapshot(journalSnapshot({
      phase: "rolling-back",
      keeperUpdate: null,
      workerFingerprint: null,
      priorDurableState: "1",
      targetDurableState: "2",
    }), HOME)!;
    expect(durable.priorDurableStateVersion).toBe(1);
    expect(durable.targetDurableStateVersion).toBe(2);
    const fixture = fakeLinuxRemote(
      durable,
      false,
      new DurableStateRollForwardRequired("Linux", 1, 2),
    );
    const errors = spyOn(console, "error").mockImplementation(() => undefined);
    try {
      await expect(_recoverLinuxDeployJournal(fixture.remote)).resolves.toMatchObject({
        kind: "roll-forward-required",
      });
    } finally {
      errors.mockRestore();
    }
    expect(fixture.calls.at(-1)).toBe("clear");
    expect(fixture.calls).toContain("prove-prior:started");
    expect(fixture.calls.some(call => call.startsWith("remove-"))).toBe(false);
  });

  test("a schema-4 Linux journal parses and still rolls back", async () => {
    const legacy = parseLinuxDeployJournalSnapshot(journalSnapshot({
      phase: "rolling-back",
      schema: "4",
      keeperUpdate: null,
      workerFingerprint: null,
    }), HOME)!;
    expect(legacy.priorDurableStateVersion).toBeNull();
    expect(legacy.targetDurableStateVersion).toBeNull();
    const fixture = fakeLinuxRemote(legacy, false);
    await expect(_recoverLinuxDeployJournal(fixture.remote)).resolves.toMatchObject({
      kind: "prior-restored",
    });
    expect(fixture.calls).toContain("prove-prior:started");
    expect(fixture.calls.at(-1)).toBe("clear");
  });

  test("the Linux prior proof only gives up on a started, locked-out release", async () => {
    const durable = parseLinuxDeployJournalSnapshot(journalSnapshot({
      phase: "rolling-back",
      keeperUpdate: null,
      workerFingerprint: null,
      priorDurableState: "1",
      targetDurableState: "2",
    }), HOME)!;
    const failing = async () => ({ exit: 1, stdout: "LoadState=not-found\n", stderr: "" });
    const sleeps = spyOn(Bun, "sleep").mockResolvedValue(undefined);
    try {
      await expect(proveLinuxPriorService(
        failing, durable, LINUX_JOURNAL_PATH, LINUX_UNIT_PATH, HOME, true,
      )).rejects.toThrow(DurableStateRollForwardRequired);
      await expect(proveLinuxPriorService(
        failing, durable, LINUX_JOURNAL_PATH, LINUX_UNIT_PATH, HOME, false,
      )).rejects.toThrow("could not prove the exact prior unit and lifecycle");
    } finally {
      sleeps.mockRestore();
    }
  });
});
