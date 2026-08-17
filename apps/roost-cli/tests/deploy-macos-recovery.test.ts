import { describe, expect, test } from "bun:test";
import {
  _decideMacosDeployRecovery,
  _isConfinedMacosReleasePath,
  _macosDeployJournalPath,
  _parseMacosDeployJournal,
  _recoverMacosDeployJournal,
} from "../src/deploy.ts";
import type {
  MacosDeployJournalV1,
  MacosDeployRecoveryRemote,
  MacosDeployTargetProof,
} from "../src/deploy.ts";

const SHA = "a".repeat(40);
const RELEASE_ROOT = "/Users/worker/RoostWorkerV2-releases";
const RELEASE_ID = `${SHA}-00000000-0000-4000-8000-000000000001`;
const RELEASE_PATH = `${RELEASE_ROOT}/${RELEASE_ID}`;
const PRIOR_PLIST = Buffer.from("<plist>prior bytes</plist>\n").toString("base64");

function journal(overrides: Partial<MacosDeployJournalV1> = {}): MacosDeployJournalV1 {
  return {
    schemaVersion: 1,
    phase: "activating",
    targetGitSha: SHA,
    targetReleasePath: RELEASE_PATH,
    priorPlistBase64: PRIOR_PLIST,
    priorPlistMode: 0o600,
    priorLifecycle: "unloaded",
    priorPid: null,
    priorDisabled: false,
    createdAt: "2026-08-16T00:00:00.000Z",
    updatedAt: "2026-08-16T00:00:01.000Z",
    ...overrides,
  };
}

function fakeRemote(
  durable: MacosDeployJournalV1,
  targetProof: MacosDeployTargetProof = {
    definitionMatches: false,
    running: false,
    result: { exit: 1, stdout: "state = exited\n", stderr: "" },
  },
  failProof = false,
): { remote: MacosDeployRecoveryRemote; calls: string[] } {
  const calls: string[] = [];
  const remote: MacosDeployRecoveryRemote = {
    async load() {
      calls.push("load");
      return durable;
    },
    async proveTarget() {
      calls.push("prove-target");
      return targetProof;
    },
    async bootout() {
      calls.push("bootout");
    },
    async restorePriorDefinition(saved) {
      calls.push(`restore-prior:${saved.priorPlistBase64 === null ? "absent" : "bytes"}`);
    },
    async setDisabled(_saved, disabled) {
      calls.push(`disabled:${disabled}`);
    },
    async bootstrap() {
      calls.push("bootstrap");
    },
    async kickstart() {
      calls.push("kickstart");
    },
    async stop() {
      calls.push("stop");
    },
    async provePrior() {
      calls.push("prove-prior");
      if (failProof) throw new Error("prior lifecycle mismatch");
    },
    async removeTarget() {
      calls.push("remove-target");
    },
    async cleanupPriorRelease() {
      calls.push("cleanup-prior");
    },
    async clear() {
      calls.push("clear");
    },
  };
  return { remote, calls };
}

describe("remote macOS deploy journal recovery", () => {
  test("uses one journal beside the machine transaction database", () => {
    expect(_macosDeployJournalPath(
      "Library/Application Support/RoostWorkerV2/service/machine-transaction.sqlite",
    )).toBe("Library/Application Support/RoostWorkerV2/service/macos-worker-deploy-v1.json");
    expect(_macosDeployJournalPath(
      "/Volumes/Secure/Roost Service/machine-transaction.sqlite",
    )).toBe("/Volumes/Secure/Roost Service/macos-worker-deploy-v1.json");
  });

  test("makes phase and health decisions fail closed", () => {
    expect(_decideMacosDeployRecovery("prepared", {
      definitionMatches: true,
      running: true,
    })).toBe("clean-prepared");
    expect(_decideMacosDeployRecovery("activating", {
      definitionMatches: true,
      running: true,
    })).toBe("commit");
    expect(_decideMacosDeployRecovery("activating", {
      definitionMatches: false,
      running: true,
    })).toBe("rollback");
    expect(_decideMacosDeployRecovery("activating", {
      definitionMatches: true,
      running: false,
    })).toBe("rollback");
  });

  test("accepts only the exact target identity directly under the managed root", () => {
    expect(_isConfinedMacosReleasePath(RELEASE_ROOT, RELEASE_PATH, SHA)).toBe(true);
    expect(_isConfinedMacosReleasePath(
      RELEASE_ROOT,
      `${RELEASE_ROOT}-attacker/${RELEASE_ID}`,
      SHA,
    )).toBe(false);
    expect(_isConfinedMacosReleasePath(
      RELEASE_ROOT,
      `${RELEASE_ROOT}/${RELEASE_ID}/../../victim`,
      SHA,
    )).toBe(false);
    expect(_isConfinedMacosReleasePath(
      RELEASE_ROOT,
      `${RELEASE_PATH}/nested`,
      SHA,
    )).toBe(false);
    expect(_isConfinedMacosReleasePath(RELEASE_ROOT, RELEASE_PATH, "b".repeat(40))).toBe(false);

    expect(_parseMacosDeployJournal(journal(), RELEASE_ROOT)).toEqual(journal());
    expect(() => _parseMacosDeployJournal({
      ...journal(),
      targetReleasePath: `/tmp/${RELEASE_ID}`,
    }, RELEASE_ROOT)).toThrow("target path or identity is malformed");
    expect(() => _parseMacosDeployJournal({
      ...journal(),
      priorPlistBase64: null,
      priorPlistMode: null,
      priorLifecycle: "running",
      priorPid: 42,
    }, RELEASE_ROOT)).toThrow("without plist bytes");
  });

  test("cleans prepared-only state without inspecting or mutating launchd", async () => {
    const fixture = fakeRemote(journal({ phase: "prepared" }));
    const result = await _recoverMacosDeployJournal(fixture.remote);

    expect(result.outcome).toBe("prepared-cleaned");
    expect(fixture.calls).toEqual(["load", "remove-target", "clear"]);
  });

  test("commits only an exact healthy target and clears last", async () => {
    const proof: MacosDeployTargetProof = {
      definitionMatches: true,
      running: true,
      result: {
        exit: 0,
        stdout: "active count = 1\nstate = running\npid = 99\nRoostReleaseMatch=yes\n",
        stderr: "",
      },
    };
    const fixture = fakeRemote(journal(), proof);
    const result = await _recoverMacosDeployJournal(fixture.remote);

    expect(result).toMatchObject({ outcome: "committed", targetProof: proof });
    expect(fixture.calls).toEqual(["load", "prove-target", "cleanup-prior", "clear"]);
  });

  test("retains the journal when prior release cleanup cannot complete", async () => {
    const fixture = fakeRemote(journal(), {
      definitionMatches: true,
      running: true,
      result: { exit: 0, stdout: "state = running\n", stderr: "" },
    });
    fixture.remote.cleanupPriorRelease = async () => {
      fixture.calls.push("cleanup-prior");
      throw new Error("prior cleanup failed");
    };
    await expect(_recoverMacosDeployJournal(fixture.remote)).rejects.toThrow("prior cleanup failed");
    expect(fixture.calls).toEqual(["load", "prove-target", "cleanup-prior"]);
  });

  test("restores a running worker, including override and process restart", async () => {
    const fixture = fakeRemote(journal({
      priorLifecycle: "running",
      priorPid: 42,
      priorDisabled: true,
    }));
    const result = await _recoverMacosDeployJournal(fixture.remote);

    expect(result.outcome).toBe("rolled-back");
    expect(fixture.calls).toEqual([
      "load",
      "prove-target",
      "bootout",
      "restore-prior:bytes",
      "disabled:false",
      "bootstrap",
      "kickstart",
      "disabled:true",
      "prove-prior",
      "remove-target",
      "clear",
    ]);
  });

  test("restores the loaded-but-not-running lifecycle exactly", async () => {
    const fixture = fakeRemote(journal({
      priorLifecycle: "loaded",
      priorPid: null,
      priorDisabled: false,
    }));
    await _recoverMacosDeployJournal(fixture.remote);

    expect(fixture.calls).toEqual([
      "load",
      "prove-target",
      "bootout",
      "restore-prior:bytes",
      "disabled:false",
      "bootstrap",
      "disabled:true",
      "stop",
      "disabled:false",
      "prove-prior",
      "remove-target",
      "clear",
    ]);
  });

  test("restores an unloaded plist without bootstrapping it", async () => {
    const fixture = fakeRemote(journal({ priorLifecycle: "unloaded", priorDisabled: true }));
    await _recoverMacosDeployJournal(fixture.remote);

    expect(fixture.calls).toEqual([
      "load",
      "prove-target",
      "bootout",
      "restore-prior:bytes",
      "disabled:true",
      "prove-prior",
      "remove-target",
      "clear",
    ]);
  });

  test("restores an absent plist and unloaded lifecycle without bootstrapping", async () => {
    const fixture = fakeRemote(journal({
      priorPlistBase64: null,
      priorPlistMode: null,
      priorLifecycle: "unloaded",
      priorPid: null,
    }));
    await _recoverMacosDeployJournal(fixture.remote);

    expect(fixture.calls).toEqual([
      "load",
      "prove-target",
      "bootout",
      "restore-prior:absent",
      "disabled:false",
      "prove-prior",
      "remove-target",
      "clear",
    ]);
  });

  test("retains the journal and target when rollback proof fails", async () => {
    const fixture = fakeRemote(journal(), undefined, true);
    await expect(_recoverMacosDeployJournal(fixture.remote)).rejects.toThrow(
      "prior lifecycle mismatch",
    );
    expect(fixture.calls).not.toContain("remove-target");
    expect(fixture.calls).not.toContain("clear");
  });
});
