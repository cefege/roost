// Prints the canonical release-matched ROOST skill with no decoration.
// Source runs read skills/roost/SKILL.md; compiled binaries use the generated
// Bun text embed so both paths preserve the same stdout bytes.

import { readFileSync } from "node:fs";
import { ROOST_SKILL_EMBED } from "./skill-embed.generated.ts";

export type RoostSkillWriter = (contents: string) => void | Promise<void>;

const ROOST_SKILL_SOURCE_URL = new URL("../../../skills/roost/SKILL.md", import.meta.url);

export function loadRoostSkillText(): string {
  return ROOST_SKILL_EMBED ?? readFileSync(ROOST_SKILL_SOURCE_URL, "utf8");
}

export async function skill(
  args: readonly string[],
  write: RoostSkillWriter = writeRoostSkillToStdout,
): Promise<void> {
  if (args.length !== 0) throw new Error("skill: accepts no arguments");
  await write(loadRoostSkillText());
}

function writeRoostSkillToStdout(contents: string): Promise<void> {
  return new Promise((resolve, reject) => {
    process.stdout.write(contents, (error) => {
      if (error) reject(error);
      else resolve();
    });
  });
}
