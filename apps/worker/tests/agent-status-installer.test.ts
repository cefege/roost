import { afterEach, describe, expect, test } from "bun:test";
import { mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  installAgentIntegrations,
  resolveOmpExtensionDir,
  resolvePiExtensionDir,
} from "../src/agent-status/install-integrations.ts";

const cleanupDirs: string[] = [];
afterEach(async () => {
  await Promise.all(cleanupDirs.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

async function tempHome(): Promise<string> {
  const path = await mkdtemp(join(tmpdir(), "roost-integrations-"));
  cleanupDirs.push(path);
  return path;
}

describe("agent integration paths", () => {
  test("resolves default and configured Pi/OMP directories", async () => {
    const home = await tempHome();
    expect(resolvePiExtensionDir({}, home)).toBe(join(home, ".pi", "agent", "extensions"));
    expect(resolveOmpExtensionDir({}, home)).toBe(join(home, ".omp", "agent", "extensions"));
    expect(resolvePiExtensionDir({ PI_CODING_AGENT_DIR: "~/shared" }, home))
      .toBe(join(home, "shared", "extensions"));
    expect(resolveOmpExtensionDir({ PI_CONFIG_DIR: "custom-omp" }, home))
      .toBe(join(home, "custom-omp", "agent", "extensions"));
    expect(resolveOmpExtensionDir({ PI_CODING_AGENT_DIR: "/tmp/shared" }, home))
      .toBe("/tmp/shared/extensions");
  });
});

describe("agent integration installation", () => {
  test("atomically installs owned assets and is idempotent", async () => {
    const home = await tempHome();
    const installed = await installAgentIntegrations({}, home);
    const omp = await readFile(installed.omp, "utf8");
    const pi = await readFile(installed.pi, "utf8");
    expect(omp).toContain("ROOST_INTEGRATION_ID=omp");
    expect(pi).toContain("ROOST_INTEGRATION_ID=pi");
    expect((await stat(installed.omp)).mode & 0o777).toBe(0o600);
    const inode = (await stat(installed.omp)).ino;
    expect(await installAgentIntegrations({}, home)).toEqual(installed);
    expect((await stat(installed.omp)).ino).toBe(inode);
    expect((await readdir(resolveOmpExtensionDir({}, home))).some((name) => name.endsWith(".tmp"))).toBe(false);
  });

  test("preserves a user file at the owned filename", async () => {
    const home = await tempHome();
    const directory = resolveOmpExtensionDir({}, home);
    const target = join(directory, "roost-omp-agent-state.ts");
    await mkdir(directory, { recursive: true });
    await writeFile(target, "// user extension\n");
    await expect(installAgentIntegrations({}, home)).rejects.toThrow("refusing to overwrite non-Roost extension");
    expect(await readFile(target, "utf8")).toBe("// user extension\n");
  });
});
