// ROOST skill tests pin the canonical markdown bytes across Bun text import,
// source fallback, and the real CLI stdout path. Argument rejection is checked
// separately so errors can never decorate or partially write the skill.

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { parse as parseYaml } from "yaml";
import { loadRoostSkillText, skill } from "../src/skill.ts";

const ROOT = resolve(import.meta.dir, "../../..");
const SKILL_PATH = resolve(ROOT, "skills/roost/SKILL.md");
const SOURCE_ENTRY = resolve(ROOT, "apps/roost-cli/src/main.ts");

describe("release-matched ROOST skill", () => {
  test("keeps canonical, Bun-imported, and source-loader bytes identical", async () => {
    const canonicalBytes = readFileSync(SKILL_PATH);
    // Runtime selection is intentional: this exercises Bun's text-import
    // boundary without teaching TypeScript that every Markdown file is code.
    const importedModule = await import(pathToFileURL(SKILL_PATH).href, {
      with: { type: "text" },
    });
    if (typeof importedModule.default !== "string") {
      throw new Error("Bun text import did not return a string");
    }
    const importedText = importedModule.default;

    expect(Buffer.from(importedText, "utf8")).toEqual(canonicalBytes);
    expect(Buffer.from(loadRoostSkillText(), "utf8")).toEqual(canonicalBytes);
  });

  test("emits only canonical bytes from the source CLI", () => {
    const canonicalBytes = readFileSync(SKILL_PATH);
    const result = Bun.spawnSync([process.execPath, SOURCE_ENTRY, "skill"], {
      cwd: ROOT,
      stdout: "pipe",
      stderr: "pipe",
    });

    expect(result.exitCode, result.stderr.toString()).toBe(0);
    expect(Buffer.from(result.stdout)).toEqual(canonicalBytes);
    expect(result.stderr.byteLength).toBe(0);
  });

  test("accepts no arguments and writes nothing when rejected", async () => {
    const written: string[] = [];
    await expect(skill(["unexpected"], (contents) => {
      written.push(contents);
    })).rejects.toThrow("skill: accepts no arguments");
    expect(written).toEqual([]);

    const result = Bun.spawnSync([process.execPath, SOURCE_ENTRY, "skill", "unexpected"], {
      cwd: ROOT,
      stdout: "pipe",
      stderr: "pipe",
    });
    expect(result.exitCode).toBe(1);
    expect(result.stdout.byteLength).toBe(0);
  });

  test("has valid discoverable skill frontmatter", () => {
    const canonical = readFileSync(SKILL_PATH, "utf8");
    const match = /^---\n([\s\S]*?)\n---\n/.exec(canonical);
    if (!match) throw new Error("canonical skill is missing YAML frontmatter");
    const metadata: unknown = parseYaml(match[1]!);
    expect(metadata).toEqual(expect.objectContaining({
      name: "roost",
      description: expect.stringMatching(/\S/),
    }));
  });
});
