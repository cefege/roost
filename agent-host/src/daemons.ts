import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { packagedDaemon } from "@earendil-works/pi-env";

const platforms = [
  ["linux", "x64", "linux-x64"], ["linux", "arm64", "linux-arm64"],
  ["darwin", "x64", "darwin-x64"], ["darwin", "arm64", "darwin-arm64"],
  ["windows", "x64", "windows-x64"], ["windows", "arm64", "windows-arm64"],
] as const;

export async function loadDaemons(): Promise<void> {
  const daemons: Record<string, { sha256: string; path: string }> = {};
  for (const [platform, arch, key] of platforms) {
    try {
      const path = packagedDaemon({ platform, arch });
      const data = await readFile(path);
      daemons[key] = { sha256: createHash("sha256").update(data).digest("hex"), path };
    } catch (error) {
      process.stdout.write(`${JSON.stringify({ level: "warn", message: "pi-env daemon unavailable", platform: key, error: String(error) })}\n`);
    }
  }
  process.env.ROOST_PI_ENV_DAEMONS = JSON.stringify(daemons);
}
