// These tests pin the Linux/root qualification profile without invoking Docker or the E2Es.
// Fakes expose prerequisite failures and prove one built image and network are shared by all scenarios.
// Cleanup assertions distinguish profile-owned names from the immutable image ID consumed by containers.

import { expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { _managedTestInternals } from "../src/test.ts";

const IMAGE_ID = `sha256:${"a".repeat(64)}`;
type ManagedDependencies = Parameters<typeof _managedTestInternals.assertManagedPrerequisites>[0];

function prerequisiteDependencies(): ManagedDependencies {
  return {
    platform: "linux",
    getuid: () => 0,
    which: (name) => `/usr/bin/${name}`,
    chromiumExecutablePath: async () => "/playwright/chromium",
    pathExists: () => true,
    createTempRoot: () => "/unused",
    removeTempRoot: () => {},
    uniqueSuffix: () => "unused",
    command: () => ({ exitCode: 0, stdout: "27.0.0", stderr: "" }),
    run: async () => {},
  };
}

test("ROOST_SAAS_E2E_GIT_SHA is the only managed build identity input", () => {
  expect(_managedTestInternals.managedE2eGitSha({})).toBe("0".repeat(40));
  expect(_managedTestInternals.managedE2eGitSha({ ROOST_GIT_SHA: "f".repeat(40) })).toBe("0".repeat(40));
  expect(_managedTestInternals.managedE2eGitSha({
    ROOST_SAAS_E2E_GIT_SHA: "A".repeat(40),
    ROOST_GIT_SHA: "f".repeat(40),
  })).toBe("a".repeat(40));
  for (const invalid of ["", "a".repeat(39), "a".repeat(41), "g".repeat(40), `${"a".repeat(40)}-dirty`]) {
    expect(() => _managedTestInternals.managedE2eGitSha({ ROOST_SAAS_E2E_GIT_SHA: invalid }))
      .toThrow("ROOST_SAAS_E2E_GIT_SHA to be exactly 40 hexadecimal characters");
  }
});

test("managed prerequisite failures name the missing boundary", async () => {
  const base = prerequisiteDependencies();
  await expect(_managedTestInternals.assertManagedPrerequisites({
    ...base,
    platform: "darwin",
  })).rejects.toThrow("requires Linux");
  await expect(_managedTestInternals.assertManagedPrerequisites({
    ...base,
    getuid: () => 1000,
  })).rejects.toThrow("requires root");
  for (const missing of ["docker", "age", "age-keygen"]) {
    await expect(_managedTestInternals.assertManagedPrerequisites({
      ...base,
      which: (name) => name === missing ? null : `/usr/bin/${name}`,
    })).rejects.toThrow(`requires ${missing}`);
  }
  await expect(_managedTestInternals.assertManagedPrerequisites({
    ...base,
    command: () => ({ exitCode: 1, stdout: "", stderr: "daemon unavailable" }),
  })).rejects.toThrow("reachable Docker daemon: daemon unavailable");
  await expect(_managedTestInternals.assertManagedPrerequisites({
    ...base,
    pathExists: () => false,
  })).rejects.toThrow("requires Playwright Chromium");
});

test("builds once, exports shared resources, and cleans only profile-owned names", async () => {
  const tempRoot = mkdtempSync(join(tmpdir(), "roost-managed-profile-test-"));
  const commands: string[][] = [];
  const runs: Array<{ name: string; cmd: string[]; env?: Record<string, string> }> = [];
  let tempRootRemoved = false;
  const dependencies: ManagedDependencies = {
    ...prerequisiteDependencies(),
    createTempRoot: () => tempRoot,
    removeTempRoot: (path) => {
      expect(path).toBe(tempRoot);
      tempRootRemoved = true;
      rmSync(path, { recursive: true, force: true });
    },
    uniqueSuffix: () => "1234-deadbeef",
    command: (cmd) => {
      commands.push(cmd);
      if (cmd[1] === "image" && cmd[2] === "inspect") {
        return { exitCode: 0, stdout: IMAGE_ID, stderr: "" };
      }
      return { exitCode: 0, stdout: "27.0.0", stderr: "" };
    },
    run: async (name, cmd, env) => {
      runs.push({ name, cmd, env });
      if (name === "managed coordinator image") {
        const iidFileIndex = cmd.indexOf("--iidfile") + 1;
        writeFileSync(cmd[iidFileIndex]!, `${IMAGE_ID}\n`);
      }
      if (name === "managed") throw new Error("scenario failed");
    },
  };

  await expect(_managedTestInternals.runManagedProfile({
    ROOST_SAAS_E2E_GIT_SHA: "A".repeat(40),
    ROOST_GIT_SHA: "f".repeat(40),
  }, dependencies)).rejects.toThrow("scenario failed");

  const buildRuns = runs.filter((invocation) => invocation.name === "managed coordinator image");
  expect(buildRuns).toHaveLength(1);
  expect(buildRuns[0]!.cmd).toContain("Dockerfile.coord");
  expect(buildRuns[0]!.cmd).toContain(`ROOST_GIT_SHA=${"a".repeat(40)}`);

  const network = "roost-managed-e2e-1234-deadbeef";
  expect(runs.find((invocation) => invocation.name === "managed Docker network")?.cmd).toEqual([
    "docker",
    "network",
    "create",
    "--label",
    "com.roost.test-managed=1234-deadbeef",
    network,
  ]);
  const managedRun = runs.find((invocation) => invocation.name === "managed");
  expect(managedRun?.cmd).toEqual([
    process.execPath,
    "test",
    "apps/roost-cli/tests/managed-browser.e2e.test.ts",
    "apps/roost-cli/tests/saas-provisioning.e2e.test.ts",
    "apps/roost-cli/tests/saas-backup-restore.e2e.test.ts",
    "apps/roost-cli/tests/saas-open-signup.e2e.test.ts",
  ]);
  expect(managedRun?.env).toEqual({
    ROOST_SAAS_E2E: "1",
    ROOST_SIGNUP_E2E: "1",
    ROOST_SAAS_E2E_IMAGE: IMAGE_ID,
    ROOST_SAAS_E2E_NETWORK: network,
    ROOST_SAAS_E2E_GIT_SHA: "a".repeat(40),
  });
  expect(commands).toContainEqual(["docker", "network", "rm", network]);
  expect(commands).toContainEqual(["docker", "image", "rm", "roost-managed-e2e:1234-deadbeef"]);
  expect(commands).not.toContainEqual(["docker", "image", "rm", IMAGE_ID]);
  expect(tempRootRemoved).toBe(true);
});

test("does not remove a network when creation never established ownership", async () => {
  const tempRoot = mkdtempSync(join(tmpdir(), "roost-managed-profile-test-"));
  const commands: string[][] = [];
  const dependencies: ManagedDependencies = {
    ...prerequisiteDependencies(),
    createTempRoot: () => tempRoot,
    removeTempRoot: (path) => rmSync(path, { recursive: true, force: true }),
    uniqueSuffix: () => "5678-collision",
    command: (cmd) => {
      commands.push(cmd);
      if (cmd[1] === "image" && cmd[2] === "inspect") {
        return { exitCode: 0, stdout: IMAGE_ID, stderr: "" };
      }
      return { exitCode: 0, stdout: "27.0.0", stderr: "" };
    },
    run: async (name, cmd) => {
      if (name === "managed coordinator image") {
        const iidFileIndex = cmd.indexOf("--iidfile") + 1;
        writeFileSync(cmd[iidFileIndex]!, `${IMAGE_ID}\n`);
      }
      if (name === "managed Docker network") throw new Error("network already exists");
    },
  };

  await expect(_managedTestInternals.runManagedProfile({}, dependencies))
    .rejects.toThrow("network already exists");
  expect(commands).not.toContainEqual([
    "docker",
    "network",
    "rm",
    "roost-managed-e2e-5678-collision",
  ]);
  expect(commands).toContainEqual([
    "docker",
    "image",
    "rm",
    "roost-managed-e2e:5678-collision",
  ]);
});
