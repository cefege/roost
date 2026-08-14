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
  test("atomically installs both retained owned assets and is idempotent", async () => {
    const home = await tempHome();
    const installed = await installAgentIntegrations({}, home);
    expect(installed).toEqual({
      omp: join(resolveOmpExtensionDir({}, home), "roost-omp-agent-state.ts"),
      pi: join(resolvePiExtensionDir({}, home), "roost-pi-agent-state.ts"),
    });
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

  test("removes an owned retired OMP session API extension before installing retained assets", async () => {
    const home = await tempHome();
    const directory = resolveOmpExtensionDir({}, home);
    const retired = join(directory, "roost-omp-" + "session-api.ts");
    await mkdir(directory, { recursive: true });
    await writeFile(retired, "// ROOST_INTEGRATION_ID=omp\n");

    const installed = await installAgentIntegrations({}, home);

    await expect(readFile(retired, "utf8")).rejects.toMatchObject({ code: "ENOENT" });
    expect(await readFile(installed.omp, "utf8")).toContain("ROOST_INTEGRATION_ID=omp");
    expect(await readFile(installed.pi, "utf8")).toContain("ROOST_INTEGRATION_ID=pi");
  });

  test("preserves an unowned file at the retired OMP session API filename", async () => {
    const home = await tempHome();
    const directory = resolveOmpExtensionDir({}, home);
    const retired = join(directory, "roost-omp-" + "session-api.ts");
    await mkdir(directory, { recursive: true });
    await writeFile(retired, "// user extension\n");

    await installAgentIntegrations({}, home);

    expect(await readFile(retired, "utf8")).toBe("// user extension\n");
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
