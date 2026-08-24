import { describe, expect, test } from "bun:test";
import {
  isManagedLinuxWorkerReleasePath,
  linuxDeployJournalPath,
  linuxDeployRecoveryPlan,
  parseLinuxDeployJournalSnapshot,
  type LinuxDeployJournal,
} from "../src/linux-deploy-journal.ts";
import { _linuxRemoveManagedWorkerReleaseCommand } from "../src/linux-deploy-journal-commands.ts";
import {
  _recoverLinuxDeployJournal,
  type LinuxDeployRecoveryRemote,
} from "../src/deploy-linux.ts";
const HOME = "/home/worker";
const SHA = "a".repeat(40);
const TARGET = `${HOME}/.local/share/roost/releases/worker/${SHA}-11111111-1111-4111-8111-111111111111`;
const PRIOR_UNIT = [
  "[Service]",
  `WorkingDirectory=${HOME}/.local/share/roost/releases/worker/prior`,
  'Environment="GIT_SHA=prior"',
  "",
].join("\n");

function encode(value: string): string {
  return Buffer.from(value, "utf8").toString("base64");
}

function journalSnapshot(options: {
  phase: string;
  target?: string;
  sha?: string;
  priorUnit?: string | null;
  lifecycle?: string;
  priorPid?: number;
}): string {
  const priorUnit = options.priorUnit === undefined ? PRIOR_UNIT : options.priorUnit;
  const lifecycle = options.lifecycle ?? "stopped";
  const fields: Record<string, string> = {
    schema: "2",
    phase: options.phase,
    "target-sha": options.sha ?? SHA,
    "target-release": options.target ?? TARGET,
    "prior-unit-state": priorUnit === null ? "absent" : "present",
    "prior-unit-mode": priorUnit === null ? "" : "644",
    "prior-lifecycle": lifecycle,
    "prior-enablement": priorUnit === null ? "absent" : "enabled",
    "prior-pid": String(options.priorPid ?? (lifecycle === "running" ? 42 : 0)),
    "prior-unit": priorUnit ?? "",
  };
  return [
    "journal",
    ...Object.entries(fields).map(([name, value]) => `${name}=${encode(value)}`),
    "",
  ].join("\n");
}

function fakeRemote(
  journal: LinuxDeployJournal,
  targetHealthy: boolean,
): { calls: string[]; remote: LinuxDeployRecoveryRemote } {
  const calls: string[] = [];
  return {
    calls,
    remote: {
      home: HOME,
      loadJournal: async () => {
        calls.push("load");
        return journal;
      },
      proveTarget: async () => {
        calls.push("prove-target");
        return {
          healthy: targetHealthy,
          proof: { exit: targetHealthy ? 0 : 1, stdout: "", stderr: "" },
        };
      },
      restorePrior: async (loaded) => {
        calls.push(
          `restore-${loaded.priorUnit === null ? "absent" : "present"}-${loaded.priorLifecycle}`,
        );
      },
      provePrior: async () => {
        calls.push("prove-prior");
      },
      cleanupPrior: async () => {
        calls.push("cleanup-prior");
      },
      removeTarget: async (loaded) => {
        calls.push(`remove-${loaded.targetReleasePath}`);
      },
      clearJournal: async () => {
        calls.push("clear");
      },
    },
  };
}

describe("durable Linux deployment journal recovery", () => {
  test("prepared state always discards only its confined target stage", async () => {
    const journal = parseLinuxDeployJournalSnapshot(journalSnapshot({
      phase: "prepared",
      lifecycle: "stopped",
    }), HOME)!;
    const fixture = fakeRemote(journal, true);
    expect(journal.priorUnit).toBe(PRIOR_UNIT);
    expect(linuxDeployRecoveryPlan(journal, true, HOME)).toEqual({
      kind: "clean-prepared",
    });
    await expect(_recoverLinuxDeployJournal(fixture.remote)).resolves.toEqual({
      kind: "prepared-cleaned",
    });
    expect(fixture.calls).toEqual(["load", `remove-${TARGET}`, "clear"]);
    expect(isManagedLinuxWorkerReleasePath(TARGET, HOME)).toBe(true);
    expect(isManagedLinuxWorkerReleasePath(`${TARGET}/nested`, HOME)).toBe(false);
  });

  test("activating or activated state commits only an independently healthy exact target", async () => {
    for (const phase of ["activating", "activated"] as const) {
      const journal = parseLinuxDeployJournalSnapshot(journalSnapshot({ phase }), HOME)!;
      const fixture = fakeRemote(journal, true);
      expect(linuxDeployRecoveryPlan(journal, true, HOME)).toEqual({
        kind: "commit-target",
      });
      await expect(_recoverLinuxDeployJournal(fixture.remote)).resolves.toMatchObject({
        kind: "target-committed",
      });
      expect(fixture.calls).toEqual(["load", "prove-target", "cleanup-prior", "clear"]);
    }
  });

  test("healthy activation retains the journal when prior release cleanup fails", async () => {
    const loaded = parseLinuxDeployJournalSnapshot(journalSnapshot({ phase: "activated" }), HOME)!;
    const fixture = fakeRemote(loaded, true);
    fixture.remote.cleanupPrior = async () => {
      fixture.calls.push("cleanup-prior");
      throw new Error("prior cleanup failed");
    };
    await expect(_recoverLinuxDeployJournal(fixture.remote)).rejects.toThrow("prior cleanup failed");
    expect(fixture.calls).toEqual(["load", "prove-target", "cleanup-prior"]);
  });

  test("unhealthy activation restores prior bytes and running lifecycle", async () => {
    const journal = parseLinuxDeployJournalSnapshot(journalSnapshot({
      phase: "activating",
      lifecycle: "running",
    }), HOME)!;
    const fixture = fakeRemote(journal, false);
    expect(linuxDeployRecoveryPlan(journal, false, HOME)).toEqual({
      kind: "rollback",
      priorUnitState: "present",
      priorLifecycle: "running",
    });
    await expect(_recoverLinuxDeployJournal(fixture.remote)).resolves.toEqual({
      kind: "prior-restored",
    });
    expect(fixture.calls).toEqual([
      "load",
      "prove-target",
      "restore-present-running",
      "prove-prior",
      `remove-${TARGET}`,
      "clear",
    ]);
  });

  test("unhealthy activation restores prior absence and stopped lifecycle", async () => {
    const journal = parseLinuxDeployJournalSnapshot(journalSnapshot({
      phase: "activated",
      priorUnit: null,
      lifecycle: "stopped",
    }), HOME)!;
    const fixture = fakeRemote(journal, false);
    expect(journal.priorUnit).toBeNull();
    expect(linuxDeployRecoveryPlan(journal, false, HOME)).toEqual({
      kind: "rollback",
      priorUnitState: "absent",
      priorLifecycle: "stopped",
    });
    await expect(_recoverLinuxDeployJournal(fixture.remote)).resolves.toEqual({
      kind: "prior-restored",
    });
    expect(fixture.calls).toEqual([
      "load",
      "prove-target",
      "restore-absent-stopped",
      "prove-prior",
      `remove-${TARGET}`,
      "clear",
    ]);
  });

  test("rollback proof failure retains both the target stage and journal", async () => {
    const journal = parseLinuxDeployJournalSnapshot(journalSnapshot({
      phase: "activating",
      lifecycle: "running",
    }), HOME)!;
    const fixture = fakeRemote(journal, false);
    fixture.remote.provePrior = async () => {
      fixture.calls.push("prove-prior");
      throw new Error("prior lifecycle mismatch");
    };
    await expect(_recoverLinuxDeployJournal(fixture.remote))
      .rejects.toThrow("prior lifecycle mismatch");
    expect(fixture.calls).toEqual([
      "load",
      "prove-target",
      "restore-present-running",
      "prove-prior",
    ]);
  });

  test("malformed state and every unconfined loaded removal path fail closed", () => {
    expect(() => parseLinuxDeployJournalSnapshot(journalSnapshot({
      phase: "committed",
    }), HOME)).toThrow("invalid phase");
    expect(() => parseLinuxDeployJournalSnapshot(journalSnapshot({
      phase: "activating",
      target: "/tmp/attacker-controlled-release",
    }), HOME)).toThrow("outside the managed worker release root");
    expect(() => parseLinuxDeployJournalSnapshot(journalSnapshot({
      phase: "activating",
      priorUnit: null,
      lifecycle: "running",
    }), HOME)).toThrow("absent prior unit cannot have a running lifecycle");
    expect(() => _linuxRemoveManagedWorkerReleaseCommand("/tmp/release", HOME))
      .toThrow("refusing to remove unmanaged worker release");
    expect(() => parseLinuxDeployJournalSnapshot(
      journalSnapshot({ phase: "activating" }).replace(
        `prior-unit-mode=${encode("644")}`,
        `prior-unit-mode=${encode("888")}`,
      ),
      HOME,
    )).toThrow("prior unit mode is malformed");
    expect(() => parseLinuxDeployJournalSnapshot(
      journalSnapshot({ phase: "activating" }).replace(
        `prior-enablement=${encode("enabled")}`,
        `prior-enablement=${encode("transient")}`,
      ),
      HOME,
    )).toThrow("prior unit enablement is malformed");
    expect(() => parseLinuxDeployJournalSnapshot(
      journalSnapshot({ phase: "activating", priorPid: 42 }),
      HOME,
    )).toThrow("process epoch and lifecycle disagree");
    expect(() => linuxDeployJournalPath("../machine-transaction.sqlite", HOME))
      .toThrow("escapes the remote home");
  });

  test("journal location is fixed beside the machine transaction database", () => {
    expect(linuxDeployJournalPath(
      ".local/share/RoostWorkerV2/service/machine-transaction.sqlite",
      HOME,
    )).toBe(`${HOME}/.local/share/RoostWorkerV2/service/worker-deploy-journal`);
    expect(linuxDeployJournalPath(
      "/srv/worker/service/machine-transaction.sqlite",
      HOME,
    )).toBe("/srv/worker/service/worker-deploy-journal");
  });
});
