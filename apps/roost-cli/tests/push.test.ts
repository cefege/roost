import { describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  ambiguousPushTargets,
  deployCoordinatorForPlatform,
  preserveWebDistForNoBuild,
  preflightWindowsFleetRelease,
  resolvePushTargets,
  workerConvergenceThresholds,
  workerVersionProblems,
} from "../src/push.ts";
import {
  coordinatorDeployRecoveryAction,
  coordinatorInstallEnvironment,
  coordinatorRestartCommand,
  coordinatorStagedReleasePathIsSafe,
  parseCoordinatorDeployJournal,
  type CoordinatorDeployJournalV1,
} from "../src/coordinator-deploy-journal.ts";
import { tryCoordinatorSelfUpdate } from "../src/deploy-windows-channel.ts";
import type { WorkerStatus } from "../src/status.ts";
import { stubFetch } from "./test-helpers.ts";

function worker(label: string, os: string, reachableAddr: string, gitSha: string | null): WorkerStatus {
  return {
    fingerprint: `${label}-fp`,
    label,
    os,
    reachableAddr,
    gitSha,
    keeperState: "current",
    keeperBuild: null,
    lastSeenMs: Date.now(),
    ageMs: 0,
    stale: false,
  };
}

describe("fleet push inventory", () => {
  const workers = [
    worker("linux-worker", "linux", "linux-worker.tailnet.ts.net", "abc1234"),
    worker("mac-worker", "darwin", "mac-worker.tailnet.ts.net", "abc1234"),
    worker("windows-worker", "win32", "windows-worker.tailnet.ts.net", "abc1234"),
  ];

  test("auto-discovers POSIX addresses and authenticated Windows identities", () => {
    expect(resolvePushTargets(undefined, workers)).toEqual([
      "linux-worker.tailnet.ts.net",
      "mac-worker.tailnet.ts.net",
      "windows-worker-fp",
    ]);
  });

  test("rejects option-shaped inventory targets before SSH", () => {
    expect(() => resolvePushTargets(undefined, [
      worker("host", "linux", "-oProxyCommand=touch-pwned", "abc1234"),
    ])).toThrow("invalid SSH deployment target");
  });

  test("routes Windows targets through their authenticated fingerprint", () => {
    expect(resolvePushTargets("windows-worker.tailnet.ts.net", workers)).toEqual([
      "windows-worker-fp",
    ]);
  });

  test("rejects ambiguous short aliases but honors an exact full address", () => {
    const aliases = [
      worker("alpha-one", "linux", "alpha.one.ts.net", "abc1234"),
      worker("alpha-two", "win32", "alpha.two.ts.net", "abc1234"),
    ];
    expect(ambiguousPushTargets(["alpha"], aliases)).toEqual(["alpha"]);
    expect(ambiguousPushTargets(["alpha.two.ts.net"], aliases)).toEqual([]);
    expect(workerVersionProblems(["alpha"], aliases, "abc1234")).toEqual([
      "alpha: ambiguous coordinator worker identity",
    ]);
    expect(resolvePushTargets("alpha.two.ts.net", aliases)).toEqual(["alpha-two-fp"]);
  });

  test("reports missing and mismatched deployed versions", () => {
    const drifted = [
      workers[0],
      worker("mac-worker", "darwin", "mac-worker.tailnet.ts.net", "oldsha"),
    ];
    expect(workerVersionProblems([
      "linux-worker.tailnet.ts.net",
      "mac-worker.tailnet.ts.net",
      "missing-worker.tailnet.ts.net",
    ], drifted, "abc1234")).toEqual([
      "mac-worker: reports oldsha, expected abc1234",
      "missing-worker.tailnet.ts.net: missing from coordinator worker inventory",
    ]);
  });

  test("rejects a stale surviving keeper after worker convergence", () => {
    const staleKeeper = {
      ...workers[0],
      keeperState: "stale" as const,
      keeperBuild: "deadbeef0000",
    };
    expect(workerVersionProblems(
      ["linux-worker.tailnet.ts.net"],
      [staleKeeper],
      "abc1234",
    )).toEqual([
      "linux-worker: keeper reports stale build deadbeef0000",
    ]);
  });

  test("requires a heartbeat emitted after deployment completion", () => {
    const deployedAt = Date.now();
    const candidate = worker("linux-worker", "linux", "linux-worker.tailnet.ts.net", "abc1234");
    candidate.lastSeenMs = deployedAt - 1;
    const boundary = new Map([["linux-worker.tailnet.ts.net", deployedAt]]);
    expect(workerVersionProblems(
      ["linux-worker.tailnet.ts.net"],
      [candidate],
      "abc1234",
      boundary,
    )).toEqual(["linux-worker: awaiting a post-deploy heartbeat"]);
    candidate.lastSeenMs = deployedAt + 1;
    expect(workerVersionProblems(
      ["linux-worker.tailnet.ts.net"],
      [candidate],
      "abc1234",
      boundary,
    )).toEqual([]);
  });

  test("records every convergence boundary after coordinator activation", () => {
    const activatedAt = 1_765_843_200_000;
    expect([...workerConvergenceThresholds([
      "WORKER-A.tailnet.ts.net.",
      "worker-b.tailnet.ts.net",
    ], activatedAt)]).toEqual([
      ["worker-a.tailnet.ts.net", activatedAt],
      ["worker-b.tailnet.ts.net", activatedAt],
    ]);
  });
});

describe("Windows fleet release preflight", () => {
  const expectedSha = "a".repeat(40);
  const manifest = JSON.stringify({
    schemaVersion: 1,
    version: "1.2.3",
    build: expectedSha,
    platform: "win32",
    arch: "x64",
    publishedAt: "2026-08-16T00:00:00.000Z",
    package: {
      name: "roost-windows-x64.zip",
      sha256: "b".repeat(64),
      size: 123,
    },
    files: ["roost.exe", "roost-win-helper.exe", "shawl.exe"].map((path, index) => ({
      path,
      sha256: String(index + 1).repeat(64),
      size: index + 1,
      authenticodeRequired: true,
    })),
    shawl: { version: "1.9.0", upstreamSha256: "c".repeat(64) },
  });
  const digest = createHash("sha256").update(manifest).digest("hex");

  test("pins one exact Windows manifest before any host mutates", async () => {
    const fetchImpl = stubFetch((url) => (url.endsWith(".sha256") ? `${digest}\n` : manifest));
    await expect(preflightWindowsFleetRelease(expectedSha, fetchImpl))
      .resolves.toEqual({ manifestSha256: digest });
  });

  test("rejects a signed-release build mismatch during fleet preflight", async () => {
    const drifted = manifest.replace(expectedSha, "d".repeat(40));
    const driftedDigest = createHash("sha256").update(drifted).digest("hex");
    const fetchImpl = stubFetch((url) => (url.endsWith(".sha256") ? `${driftedDigest}\n` : drifted));
    await expect(preflightWindowsFleetRelease(expectedSha, fetchImpl))
      .rejects.toThrow(`expected source commit ${expectedSha}`);
  });
});

describe("Windows coordinator fleet update", () => {
  const sha = "a".repeat(40);

  test("leaves POSIX coordinator deployment to the source transaction", async () => {
    expect(await tryCoordinatorSelfUpdate(sha, { platform: "linux" })).toBeNull();
  });

  test("does not dispatch a second transaction when the worker update already advanced the coordinator", async () => {
    let starts = 0;
    const lines: string[] = [];
    const result = await tryCoordinatorSelfUpdate(sha, {
      platform: "win32",
      current: async () => true,
      start: async () => {
        starts += 1;
        return { jobId: "unexpected", frames: [] };
      },
      log: (line) => lines.push(line),
    });
    expect(result).toBeTrue();
    expect(starts).toBe(0);
    expect(lines).toEqual(["Windows coordinator already reports the exact target build"]);
  });

  test("waits for durable terminal success instead of treating START admission as completion", async () => {
    let statusCalls = 0;
    const lines: string[] = [];
    const result = await tryCoordinatorSelfUpdate(sha, {
      platform: "win32",
      current: async () => false,
      start: async () => ({
        jobId: "job-1",
        frames: [{
          sequence: 1, phase: "prepared", message: "journal committed",
          terminal: false, success: false, error: "",
        }],
      }),
      status: async (_jobId, afterSequence) => {
        statusCalls += 1;
        expect(afterSequence).toBe(1);
        return [{
          sequence: 2, phase: "committed", message: "health proven",
          terminal: true, success: true, error: "",
        }];
      },
      sleep: async () => {},
      log: (line) => lines.push(line),
      prove: async () => true,
    });

    expect(result).toBeTrue();
    expect(statusCalls).toBe(1);
    expect(lines).toEqual([
      ">> [prepared] journal committed",
      ">> [committed] health proven",
    ]);
  });

  test("requires the local coordinator API to report the exact build after terminal success", async () => {
    await expect(tryCoordinatorSelfUpdate(sha, {
      platform: "win32",
      current: async () => false,
      start: async () => ({
        jobId: "job-health",
        frames: [{
          sequence: 1, phase: "committed", message: "broker completed",
          terminal: true, success: true, error: "",
        }],
      }),
      status: async () => [],
      prove: async () => false,
      log: () => {},
    })).rejects.toThrow(`did not report healthy build ${sha}`);
  });

  test("push coordinator branch does not enter POSIX deployment after Windows success", async () => {
    let posixCalls = 0;
    expect(await deployCoordinatorForPlatform(sha, true, {
      windows: async () => true,
      posix: async () => { posixCalls += 1; },
    })).toBe("windows");
    expect(posixCalls).toBe(0);
  });

  test("push coordinator branch falls back to POSIX only when Windows is inapplicable", async () => {
    let posixBuildWeb: boolean | null = null;
    expect(await deployCoordinatorForPlatform(sha, false, {
      windows: async () => null,
      posix: async (_expectedSha, buildWeb) => { posixBuildWeb = buildWeb; },
    })).toBe("posix");
    expect(posixBuildWeb).toBeFalse();
  });

  test("propagates a durable rollback result as rollout failure", async () => {
    await expect(tryCoordinatorSelfUpdate(sha, {
      platform: "win32",
      current: async () => false,
      start: async () => ({
        jobId: "job-2",
        frames: [{
          sequence: 3, phase: "rolled-back", message: "prior build restored",
          terminal: true, success: false, error: "forward health failed",
        }],
      }),
      status: async () => [],
      log: () => {},
    })).rejects.toThrow("forward health failed");
  });
});

describe("coordinator restart identity", () => {
  test("uses the configured Linux unit rather than the default identity", () => {
    const command = coordinatorRestartCommand("/tmp/custom.service", "linux", "custom-coord");
    expect(command).toContain("restart 'custom-coord.service'");
    expect(command).not.toContain("roost-coordinator-v2");
  });

  test("uses the configured macOS label for bootout, enable, and kickstart", () => {
    const command = coordinatorRestartCommand(
      "/Users/test/Library/LaunchAgents/custom.plist", "darwin", "org.example.custom",
    );
    expect(command).toContain("gui/$uid/'org.example.custom'");
    expect(command).not.toContain("com.cefege.roost.coordinator-v2");
  });
});

describe("coordinator deploy journal", () => {
  const priorSha = "1".repeat(40);
  const targetSha = "2".repeat(40);
  const releaseSuffix = "12345678-1234-4123-8123-123456789abc";
  const root = join(tmpdir(), "roost-push-journal-fixture");
  const releaseRoot = join(root, "service", "releases", "coord");
  const servicePath = join(root, "roost-coord.service");
  const sourceReleasePath = join(root, "source");
  const stagingRepoPath = join(root, "staging-repo");
  const stagedReleasePath = join(releaseRoot, `${targetSha}-${releaseSuffix}`);
  const priorDefinition = [
    "[Service]",
    `WorkingDirectory=${sourceReleasePath}`,
    `Environment="ROOST_GIT_SHA=${priorSha}"`,
  ].join("\n");
  const journal: CoordinatorDeployJournalV1 = {
    schemaVersion: 1,
    phase: "prepared",
    priorDefinitionBase64: Buffer.from(priorDefinition).toString("base64"),
    priorDefinitionMode: 0o600,
    priorSha,
    targetSha,
    servicePath,
    sourceReleasePath,
    stagingRepoPath,
    stagedReleasePath,
  };
  const context = { servicePath, releaseRoot, platform: "linux" as const };

  test("parses a complete validated recovery record", () => {
    expect(parseCoordinatorDeployJournal(JSON.stringify(journal), context)).toEqual(journal);
  });

  test("rejects a journal that cannot restore the prior definition mode", () => {
    expect(() => parseCoordinatorDeployJournal(
      JSON.stringify({ ...journal, priorDefinitionMode: 0o1000 }),
      context,
    )).toThrow("priorDefinitionMode is invalid");
  });

  test("confines the staged target to one SHA-addressed release directory", () => {
    expect(coordinatorStagedReleasePathIsSafe(releaseRoot, stagedReleasePath, targetSha)).toBeTrue();
    expect(coordinatorStagedReleasePathIsSafe(
      releaseRoot,
      join(releaseRoot, "nested", `${targetSha}-${releaseSuffix}`),
      targetSha,
    )).toBeFalse();
    const outside = join(root, "outside", `${targetSha}-${releaseSuffix}`);
    expect(coordinatorStagedReleasePathIsSafe(releaseRoot, outside, targetSha)).toBeFalse();
    expect(() => parseCoordinatorDeployJournal(
      JSON.stringify({ ...journal, stagedReleasePath: outside }),
      context,
    )).toThrow(/outside the coordinator release root/);
  });

  test("fails closed on malformed or internally inconsistent state", () => {
    expect(() => parseCoordinatorDeployJournal("{", context)).toThrow(/not valid JSON/);
    expect(() => parseCoordinatorDeployJournal(
      JSON.stringify({ ...journal, servicePath: join(root, "other.service") }),
      context,
    )).toThrow(/does not match the installed coordinator service/);
    expect(() => parseCoordinatorDeployJournal(
      JSON.stringify({ ...journal, sourceReleasePath: join(root, "other-source") }),
      context,
    )).toThrow(/does not match sourceReleasePath/);
    expect(() => parseCoordinatorDeployJournal(
      JSON.stringify({ ...journal, priorSha: "3".repeat(40) }),
      context,
    )).toThrow(/does not match priorSha/);
  });

  test("chooses deterministic recovery at every durable phase boundary", () => {
    expect(coordinatorDeployRecoveryAction("prepared", false)).toBe("clean-prepared");
    expect(coordinatorDeployRecoveryAction("prepared", true)).toBe("clean-prepared");
    expect(coordinatorDeployRecoveryAction("activating", true)).toBe("commit-target");
    expect(coordinatorDeployRecoveryAction("activated", true)).toBe("commit-target");
    expect(coordinatorDeployRecoveryAction("activating", false)).toBe("rollback-prior");
    expect(coordinatorDeployRecoveryAction("activated", false)).toBe("rollback-prior");
  });
});

describe("staged coordinator preservation", () => {
  test("reuses the installed SPA without retaining the prior release", () => {
    const root = mkdtempSync(join(tmpdir(), "roost-no-web-"));
    const release = join(root, "release");
    const embedded = join(root, "installed-spa");
    const priorRepo = join(root, "prior-repo");
    mkdirSync(embedded, { recursive: true });
    writeFileSync(join(embedded, "index.html"), "installed-spa");
    try {
      const selected = preserveWebDistForNoBuild(
        release,
        { ROOST_WEB_DIST_PATH: embedded },
        priorRepo,
      );
      rmSync(embedded, { recursive: true, force: true });
      expect(selected).toBe(join(release, "apps", "web", "dist"));
      expect(readFileSync(join(selected, "index.html"), "utf8")).toBe("installed-spa");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("keeps tracked staged dist and falls back to the prior checkout dist", () => {
    const root = mkdtempSync(join(tmpdir(), "roost-no-web-tracked-"));
    const release = join(root, "release");
    const stagedDist = join(release, "apps", "web", "dist");
    const priorRepo = join(root, "prior");
    mkdirSync(stagedDist, { recursive: true });
    mkdirSync(join(priorRepo, "apps", "web", "dist"), { recursive: true });
    writeFileSync(join(stagedDist, "index.html"), "tracked");
    writeFileSync(join(priorRepo, "apps", "web", "dist", "index.html"), "prior");
    try {
      preserveWebDistForNoBuild(release, {}, priorRepo);
      expect(readFileSync(join(stagedDist, "index.html"), "utf8")).toBe("tracked");
      rmSync(stagedDist, { recursive: true, force: true });
      preserveWebDistForNoBuild(release, {}, priorRepo);
      expect(readFileSync(join(stagedDist, "index.html"), "utf8")).toBe("prior");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("recovers direct mode and effective systemd resource limits", () => {
    const definition = [
      "[Service]",
      'Environment="ROOST_COORDINATOR_BIND=0.0.0.0:4102"',
      'Environment="ROOST_COORDINATOR_DB=/srv/state/coordinator.db"',
      'Environment="ROOST_COORD_LOGROTATE_CONF=/srv/config%%blue/coord.conf"',
      "MemoryHigh=3G",
      "MemoryMax=6G",
      "TasksMax=768",
    ].join("\n");
    expect(coordinatorInstallEnvironment(definition, "linux")).toMatchObject({
      ROOST_COORDINATOR_DB: "/srv/state/coordinator.db",
      ROOST_FRONTED: "0",
      ROOST_COORD_MEMORY_HIGH: "3G",
      ROOST_COORD_MEMORY_MAX: "6G",
      ROOST_COORD_TASKS_MAX: "768",
      ROOST_COORD_LOGROTATE_CONF: "/srv/config%blue/coord.conf",
    });
  });
});
