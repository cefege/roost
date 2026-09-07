// macOS journal schema-v2 tests pin the local/remote keeper-update byte contract
// and require the authenticated action before the worker stop boundary.
// The remote Bun program is exercised only in its journal-load mode.

import { afterEach, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  _parseMacosDeployJournal,
  type MacosDeployJournalV2,
} from "../src/deploy-macos-journal.ts";
import { createMacosDeployJournalController } from "../src/deploy-macos-journal-controller.ts";
import { MACOS_DEPLOY_JOURNAL_PROGRAM } from "../src/macos-deploy-journal-program.ts";
import {
  MACOS_KEEPER_UPDATE,
  MACOS_SOURCE_SHA,
  MACOS_WORKER_FINGERPRINT,
} from "./deploy-macos-keeper-update-fixture.ts";

const SHA = "a".repeat(40);
const temporaryRoots: string[] = [];

function journalFixture(root: string): MacosDeployJournalV2 {
  return {
    schemaVersion: 2,
    phase: "activating",
    targetGitSha: SHA,
    targetReleasePath: join(root, `${SHA}-00000000-0000-4000-8000-000000000001`),
    rolloutId: null,
    workerFingerprint: MACOS_WORKER_FINGERPRINT,
    keeperUpdate: MACOS_KEEPER_UPDATE,
    priorPlistBase64: Buffer.from(
      `<plist><dict><key>EnvironmentVariables</key><dict>`
        + `<key>GIT_SHA</key><string>${MACOS_SOURCE_SHA}</string>`
        + `</dict></dict></plist>\n`,
    ).toString("base64"),
    priorPlistMode: 0o600,
    priorLifecycle: "running",
    priorPid: 37,
    priorDisabled: false,
    createdAt: "2026-09-05T00:00:00.000Z",
    updatedAt: "2026-09-05T00:00:01.000Z",
  };
}

function loadWithRemoteProgram(value: unknown, releaseRoot: string): {
  exitCode: number;
  stdout: string;
  stderr: string;
} {
  const home = mkdtempSync(join(tmpdir(), "roost-macos-journal-"));
  temporaryRoots.push(home);
  const journalPath = join(home, "journal.json");
  writeFileSync(journalPath, `${JSON.stringify(value)}\n`);
  const result = Bun.spawnSync([process.execPath, "-e", MACOS_DEPLOY_JOURNAL_PROGRAM], {
    env: {
      ...process.env,
      ROOST_MAC_DEPLOY_ACTION: "load",
      ROOST_MAC_DEPLOY_JOURNAL: journalPath,
      ROOST_MAC_DEPLOY_RELEASE_ROOT: releaseRoot,
      ROOST_MAC_DEPLOY_PLIST: join(home, "worker.plist"),
      ROOST_MAC_DEPLOY_LABEL: "com.roost.worker-v2",
      ROOST_MAC_DEPLOY_ROLLOUT_ID: "",
      ROOST_MAC_DEPLOY_WORKER_FINGERPRINT: "",
      ROOST_MAC_DEPLOY_KEEPER_UPDATE: Buffer.from("null").toString("base64"),
    },
    stdout: "pipe",
    stderr: "pipe",
  });
  return {
    exitCode: result.exitCode ?? 1,
    stdout: new TextDecoder().decode(result.stdout),
    stderr: new TextDecoder().decode(result.stderr),
  };
}

afterEach(() => {
  for (const root of temporaryRoots.splice(0)) rmSync(root, { recursive: true, force: true });
});

describe("macOS journal keeper-update contract", () => {
  test("local and remote parsers return identical schema-v2 bytes", () => {
    const root = mkdtempSync(join(tmpdir(), "roost-macos-release-root-"));
    temporaryRoots.push(root);
    const fixture = journalFixture(root);
    mkdirSync(fixture.targetReleasePath);
    const local = _parseMacosDeployJournal(fixture, root);
    const remote = loadWithRemoteProgram(fixture, root);
    expect(remote.exitCode).toBe(0);
    const encoded = remote.stdout.trim().slice("RoostMacDeployJournal=".length);
    const envelope = JSON.parse(Buffer.from(encoded, "base64").toString("utf8"));
    expect(JSON.stringify(envelope.journal)).toBe(JSON.stringify(local));
  });
  test("a prior service with no journaled keeper update parses on both sides", () => {
    const root = mkdtempSync(join(tmpdir(), "roost-macos-release-root-"));
    temporaryRoots.push(root);
    const bootstrap = {
      ...journalFixture(root),
      workerFingerprint: null,
      keeperUpdate: null,
    };
    mkdirSync(bootstrap.targetReleasePath);

    const local = _parseMacosDeployJournal(bootstrap, root);
    expect(local.keeperUpdate).toBeNull();
    expect(local.priorPlistBase64).toBe(bootstrap.priorPlistBase64);
    const remote = loadWithRemoteProgram(bootstrap, root);
    expect(remote.exitCode).toBe(0);
    const envelope = JSON.parse(Buffer.from(
      remote.stdout.trim().slice("RoostMacDeployJournal=".length),
      "base64",
    ).toString("utf8"));
    expect(JSON.stringify(envelope.journal)).toBe(JSON.stringify(local));
  });


  test("old, missing, inconsistent, and bootstrap-only admission shapes fail closed", () => {
    const root = mkdtempSync(join(tmpdir(), "roost-macos-release-root-"));
    temporaryRoots.push(root);
    const fixture = journalFixture(root);
    mkdirSync(fixture.targetReleasePath);
    const malformed = [
      { ...fixture, schemaVersion: 1 },
      Object.fromEntries(Object.entries(fixture).filter(([name]) => name !== "keeperUpdate")),
      { ...fixture, keeperUpdate: null },
      { ...fixture, workerFingerprint: "not-a-fingerprint" },
      {
        ...fixture,
        keeperUpdate: {
          ...MACOS_KEEPER_UPDATE,
          admission: { ...MACOS_KEEPER_UPDATE.admission, required_action: "replace-empty" },
        },
      },
      {
        ...fixture,
        priorPlistBase64: null,
        priorPlistMode: null,
        priorLifecycle: "unloaded",
        priorPid: null,
      },
    ];

    for (const candidate of malformed) {
      expect(() => _parseMacosDeployJournal(candidate, root)).toThrow();
      const remote = loadWithRemoteProgram(candidate, root);
      expect(remote.exitCode).toBe(65);
      expect(remote.stderr).not.toBe("");
    }
  });
  test("target activation applies the journaled action before stop and bootstrap", async () => {
    const root = "/Users/worker/RoostWorkerV2-releases";
    const fixture = journalFixture(root);
    const calls: string[] = [];
    const controller = createMacosDeployJournalController(async (command) => {
      if (command.includes("launchctl bootout")) calls.push("bootout");
      else if (command.startsWith("launchctl enable")) calls.push("enable");
      else if (command.includes("launchctl bootstrap")) calls.push("bootstrap");
      else if (command.startsWith("launchctl kickstart")) calls.push("kickstart");
      return { exit: 0, stdout: "", stderr: "" };
    }, ".roost/transactions/macos-worker-deploy-v1.json", {
      applyKeeperUpdate: async (_workerFingerprint, update, direction, actionReleasePath) => { expect(update).toEqual(MACOS_KEEPER_UPDATE);
      expect(actionReleasePath).toBe(fixture.targetReleasePath);
      calls.push(`keeper:${direction}:${update.admission.required_action}`); },
    });

    await controller.activateTarget(fixture);
    expect(calls).toEqual([
      "keeper:target:preserve",
      "bootout",
      "enable",
      "bootstrap",
      "kickstart",
    ]);
  });

  test("an installed update cannot bootstrap when its action callback is absent", async () => {
    const fixture = journalFixture("/Users/worker/RoostWorkerV2-releases");
    const commands: string[] = [];
    const controller = createMacosDeployJournalController(async (command) => {
      commands.push(command);
      return { exit: 0, stdout: "", stderr: "" };
    }, ".roost/transactions/macos-worker-deploy-v1.json");

    await expect(controller.activateTarget(fixture)).rejects.toThrow("action is unavailable");
    expect(commands.some((command) => command.includes("launchctl bootstrap"))).toBe(false);
    expect(commands.some((command) => command.includes("launchctl bootout"))).toBe(false);
  });
});
