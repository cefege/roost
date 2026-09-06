// Pins the typed OMP/Pi integration asset set and staged installer transaction.
// Case/path aliases and ownership races fail closed; injected commit failures
// preserve user files and roll every completed asset mutation back.
import { afterEach, describe, expect, test } from "bun:test";
import {
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rm,
  stat,
  symlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  _loadAgentIntegrationAssets,
  _installAgentIntegrationsForTest,
  installAgentIntegrations,
  resolveOmpExtensionDir,
  resolvePiExtensionDir,
} from "../src/agent-status/install-integrations.ts";
import type { InstalledAgentIntegration } from "../src/agent-status/install-integrations.ts";

const cleanupDirs: string[] = [];
afterEach(async () => {
  await Promise.all(cleanupDirs.splice(0).map((path) =>
    rm(path, { recursive: true, force: true })
  ));
});

async function tempHome(): Promise<string> {
  const path = await mkdtemp(join(tmpdir(), "roost-integrations-"));
  cleanupDirs.push(path);
  return path;
}

function installedPaths(
  installed: readonly InstalledAgentIntegration[],
): Map<string, string> {
  return new Map(installed.map(({ id, path }) => [id, path]));
}

describe("agent integration paths", () => {
  test("resolves default and configured Pi/OMP directories", async () => {
    const home = await tempHome();
    expect(resolvePiExtensionDir({}, home)).toBe(
      join(home, ".pi", "agent", "extensions"),
    );
    expect(resolveOmpExtensionDir({}, home)).toBe(
      join(home, ".omp", "agent", "extensions"),
    );
    expect(resolvePiExtensionDir({ PI_CODING_AGENT_DIR: "~/shared" }, home))
      .toBe(join(home, "shared", "extensions"));
    expect(resolveOmpExtensionDir({ PI_CONFIG_DIR: "custom-omp" }, home))
      .toBe(join(home, "custom-omp", "agent", "extensions"));
    expect(resolveOmpExtensionDir({ PI_CODING_AGENT_DIR: "/tmp/shared" }, home))
      .toBe("/tmp/shared/extensions");
  });
});

describe("agent integration installation", () => {
  test("installs the complete typed assets byte-for-byte and is idempotent", async () => {
    const home = await tempHome();
    const materialized = await _loadAgentIntegrationAssets();
    const installed = await installAgentIntegrations({}, home);
    const paths = installedPaths(installed);
    expect(installed.map(({ id }) => id)).toEqual([
      "omp-status",
      "omp-reference",
      "pi-status",
    ]);
    for (const asset of materialized) {
      expect(await readFile(paths.get(asset.spec.id)!, "utf8")).toBe(asset.content);
    }
    const ompStatus = paths.get("omp-status")!;
    expect((await stat(ompStatus)).mode & 0o777).toBe(0o600);
    const inode = (await stat(ompStatus)).ino;
    expect(await installAgentIntegrations({}, home)).toEqual(installed);
    expect((await stat(ompStatus)).ino).toBe(inode);
    expect((await readdir(resolveOmpExtensionDir({}, home))).some((name) =>
      name.endsWith(".tmp")
    )).toBe(false);
  });

  test("removes an owned retired OMP asset only after successful preflight", async () => {
    const home = await tempHome();
    const directory = resolveOmpExtensionDir({}, home);
    const retired = join(directory, "roost-omp-session-api.ts");
    await mkdir(directory, { recursive: true });
    await writeFile(retired, "// ROOST_INTEGRATION_ID=omp\n");

    const paths = installedPaths(await installAgentIntegrations({}, home));

    await expect(readFile(retired, "utf8")).rejects.toMatchObject({ code: "ENOENT" });
    expect(await readFile(paths.get("omp-reference")!, "utf8"))
      .toContain("ROOST_INTEGRATION_ID=omp-reference");
  });

  test("preserves an unowned file at the retired filename", async () => {
    const home = await tempHome();
    const directory = resolveOmpExtensionDir({}, home);
    const retired = join(directory, "roost-omp-session-api.ts");
    await mkdir(directory, { recursive: true });
    await writeFile(retired, "// user extension\n");

    await installAgentIntegrations({}, home);

    expect(await readFile(retired, "utf8")).toBe("// user extension\n");
  });

  test("rejects a direct OMP/Pi destination collision without mutation", async () => {
    const home = await tempHome();
    await expect(installAgentIntegrations({
      PI_CODING_AGENT_DIR: join(home, "shared"),
    }, home)).rejects.toThrow("colliding OMP and Pi integration directories");
    expect(await readdir(home)).toEqual([]);
  });

  test("rejects a symlink directory alias without installing either runtime", async () => {
    if (process.platform === "win32") return;
    const home = await tempHome();
    const ompDirectory = resolveOmpExtensionDir({}, home);
    const piDirectory = resolvePiExtensionDir({}, home);
    await mkdir(ompDirectory, { recursive: true });
    await mkdir(join(piDirectory, ".."), { recursive: true });
    await symlink(ompDirectory, piDirectory, "dir");

    await expect(installAgentIntegrations({}, home)).rejects.toThrow(
      "colliding OMP and Pi integration directories",
    );
    expect(await readdir(ompDirectory)).toEqual([]);
  });

  test("rejects a final user-owned destination before writing earlier assets", async () => {
    const home = await tempHome();
    const piTarget = join(resolvePiExtensionDir({}, home), "roost-pi-agent-state.ts");
    await mkdir(resolvePiExtensionDir({}, home), { recursive: true });
    await writeFile(piTarget, "// user extension\n");

    await expect(installAgentIntegrations({}, home)).rejects.toThrow(
      "refusing to overwrite non-Roost extension",
    );
    expect(await readFile(piTarget, "utf8")).toBe("// user extension\n");
    await expect(readFile(
      join(resolveOmpExtensionDir({}, home), "roost-omp-agent-state.ts"),
      "utf8",
    )).rejects.toMatchObject({ code: "ENOENT" });
  });

  test("rejects an owned-filename symlink without changing its target", async () => {
    if (process.platform === "win32") return;
    const home = await tempHome();
    const outside = join(home, "user-extension.ts");
    const target = join(
      resolveOmpExtensionDir({}, home),
      "roost-omp-agent-state.ts",
    );
    await writeFile(outside, "// user extension\n");
    await mkdir(resolveOmpExtensionDir({}, home), { recursive: true });
    await symlink(outside, target);

    await expect(installAgentIntegrations({}, home)).rejects.toThrow(
      "refusing symlink agent integration target",
    );
    expect(await readFile(outside, "utf8")).toBe("// user extension\n");
    await expect(readFile(
      join(resolveOmpExtensionDir({}, home), "roost-omp-agent-reference.ts"),
      "utf8",
    )).rejects.toMatchObject({ code: "ENOENT" });
  });

  test("rejects absent case-only runtime aliases on Darwin and Windows", async () => {
    for (const platform of ["darwin", "win32"] as const) {
      const home = await tempHome();
      await expect(_installAgentIntegrationsForTest(
        { PI_CONFIG_DIR: ".PI" },
        home,
        { platform },
      )).rejects.toThrow("colliding OMP and Pi integration directories");
      expect(await readdir(home)).toEqual([]);
    }
  });

  test("preserves an unowned target that appears after staging", async () => {
    const home = await tempHome();
    const racedTarget = join(
      resolveOmpExtensionDir({}, home),
      "roost-omp-agent-state.ts",
    );
    const userContent = "// raced user extension\n";

    await expect(_installAgentIntegrationsForTest({}, home, {
      beforeFinalValidation: async () => {
        await writeFile(racedTarget, userContent, { flag: "wx" });
      },
    })).rejects.toThrow("target changed before commit");

    expect(await readFile(racedTarget, "utf8")).toBe(userContent);
    await expect(readFile(
      join(resolveOmpExtensionDir({}, home), "roost-omp-agent-reference.ts"),
      "utf8",
    )).rejects.toMatchObject({ code: "ENOENT" });
    await expect(readFile(
      join(resolvePiExtensionDir({}, home), "roost-pi-agent-state.ts"),
      "utf8",
    )).rejects.toMatchObject({ code: "ENOENT" });
  });

  test("rejects a loader symlink swap at the final mutation boundary", async () => {
    if (process.platform === "win32") return;
    const home = await tempHome();
    const firstDirectory = join(home, "omp-first");
    const racedDirectory = join(home, "omp-raced");
    const ompDirectory = resolveOmpExtensionDir({}, home);
    await mkdir(firstDirectory);
    await mkdir(racedDirectory);
    await mkdir(join(ompDirectory, ".."), { recursive: true });
    await symlink(firstDirectory, ompDirectory, "dir");

    await expect(_installAgentIntegrationsForTest({}, home, {
      beforeFinalValidation: async () => {
        await rm(ompDirectory);
        await symlink(racedDirectory, ompDirectory, "dir");
      },
    })).rejects.toThrow("loader changed during installation");

    expect(await readdir(firstDirectory)).toEqual([]);
    expect(await readdir(racedDirectory)).toEqual([]);
  });

  test("rolls back owned replacements and absent creates after a commit failure", async () => {
    const home = await tempHome();
    const ompDirectory = resolveOmpExtensionDir({}, home);
    const statusTarget = join(ompDirectory, "roost-omp-agent-state.ts");
    const referenceTarget = join(ompDirectory, "roost-omp-agent-reference.ts");
    const retiredTarget = join(ompDirectory, "roost-omp-session-api.ts");
    const priorOwnedContent = "// ROOST_INTEGRATION_ID=omp\n// prior version\n";
    const userContent = "// user extension\n";
    await mkdir(ompDirectory, { recursive: true });
    await writeFile(statusTarget, priorOwnedContent);
    await writeFile(retiredTarget, userContent);

    await expect(_installAgentIntegrationsForTest({}, home, {
      afterCommittedMutation: (completedMutations) => {
        if (completedMutations === 2) {
          throw new Error("injected integration commit failure");
        }
      },
    })).rejects.toThrow("injected integration commit failure");

    expect(await readFile(statusTarget, "utf8")).toBe(priorOwnedContent);
    expect(await readFile(retiredTarget, "utf8")).toBe(userContent);
    await expect(readFile(referenceTarget, "utf8"))
      .rejects.toMatchObject({ code: "ENOENT" });
    await expect(readFile(
      join(resolvePiExtensionDir({}, home), "roost-pi-agent-state.ts"),
      "utf8",
    )).rejects.toMatchObject({ code: "ENOENT" });
    expect((await readdir(ompDirectory)).sort()).toEqual([
      "roost-omp-agent-state.ts",
      "roost-omp-session-api.ts",
    ]);
  });
});
