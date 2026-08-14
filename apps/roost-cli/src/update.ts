// `roost update` — self-update the single binary from the latest GitHub Release
// (herdr's `update`). Retires the git-pull deploy path for binary installs.
// Pure decision (needsUpdate) + orchestration (runUpdate) are injectable so
// they're unit-tested without a network; the `update` wrapper wires the real
// GitHub fetch + atomic self-replace.
import { createHash } from "node:crypto";
import { once } from "node:events";
import { createWriteStream, renameSync } from "node:fs";
import { chmod, rm } from "node:fs/promises";
import { basename } from "node:path";
import { finished } from "node:stream/promises";
import { ROOST_VERSION } from "./version.ts";
import { currentServiceOs, restartCoordCmd, restartWorkerCmd } from "./service-ctl.ts";

const REPO = "cefege/roost";

/** Release asset for a platform/arch pair. `roost` stays unsuffixed for
 *  darwin-arm64 so existing installs' `roost update` keeps resolving; every
 *  other target is explicit. install-binary.sh's `case` mirrors this exactly —
 *  change both together or the installer 404s. */
export function releaseAssetName(platform: string = process.platform, arch: string = process.arch): string {
  if (platform === "darwin" && arch === "arm64") return "roost";
  if (platform === "darwin" && arch === "x64") return "roost-darwin-x64";
  if (platform === "linux" && arch === "x64") return "roost-linux-x64";
  if (platform === "linux" && arch === "arm64") return "roost-linux-arm64";
  throw new Error(`no prebuilt roost binary for ${platform}/${arch}`);
}

/** Download a release binary, stream-hash it, and leave an executable candidate
 * only when its separately published checksum matches exactly. */
export async function downloadVerifiedReleaseAsset(asset: string, destPath: string): Promise<void> {
  const baseUrl = `https://github.com/${REPO}/releases/latest/download`;
  await rm(destPath, { force: true });
  try {
    const checksumResponse = await fetch(`${baseUrl}/${asset}.sha256`, {
      signal: AbortSignal.timeout(10_000),
    });
    if (!checksumResponse.ok) {
      throw new Error(`checksum download failed: HTTP ${checksumResponse.status}`);
    }
    const checksumText = await checksumResponse.text();
    const checksumMatch = /^([0-9a-f]{64})[ \t\r\n\v\f]*$/.exec(checksumText);
    if (!checksumMatch) throw new Error(`invalid checksum file for ${asset}`);

    const binaryResponse = await fetch(`${baseUrl}/${asset}`, {
      signal: AbortSignal.timeout(120_000),
    });
    if (!binaryResponse.ok) throw new Error(`download failed: HTTP ${binaryResponse.status}`);
    if (!binaryResponse.body) throw new Error(`download failed: empty response for ${asset}`);

    const hash = createHash("sha256");
    const output = createWriteStream(destPath, { flags: "w", mode: 0o600 });
    const reader = binaryResponse.body.getReader();
    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        hash.update(value);
        if (!output.write(value)) await once(output, "drain");
      }
      output.end();
      await finished(output);
    } catch (error) {
      output.destroy();
      throw error;
    } finally {
      reader.releaseLock();
    }

    if (hash.digest("hex") !== checksumMatch[1]) {
      throw new Error(`checksum mismatch for ${asset}`);
    }
    await chmod(destPath, 0o755);
  } catch (error) {
    await rm(destPath, { force: true });
    throw error;
  }
}

/** Compare the running version to a release tag. "dev" (from-source) is always
 *  behind; build metadata (+sha) is ignored so a rebuild of the same release
 *  doesn't self-update forever. Empty tag (no release yet) → never. */
export function needsUpdate(current: string, latestTag: string): boolean {
  if (!latestTag) return false;
  if (current === "dev") return true;
  const norm = (s: string) => s.replace(/^v/, "").split("+")[0];
  return norm(current) !== norm(latestTag);
}

export interface UpdateDeps {
  currentVersion: string;
  execPath: string;
  fetchLatestTag: () => Promise<string>;
  downloadBinary: (destPath: string) => Promise<void>;
  replaceSelf: (fromPath: string) => void;
  log: (m: string) => void;
}

export async function runUpdate(deps: UpdateDeps): Promise<{ updated: boolean; to: string }> {
  deps.log(`current version: ${deps.currentVersion}`);
  const latest = await deps.fetchLatestTag();
  if (!latest) {
    deps.log("no published release found — nothing to update to.");
    return { updated: false, to: "" };
  }
  if (!needsUpdate(deps.currentVersion, latest)) {
    deps.log(`already up to date (${latest}).`);
    return { updated: false, to: latest };
  }
  deps.log(`updating ${deps.currentVersion} → ${latest} …`);
  const tmp = `${deps.execPath}.new`;
  await deps.downloadBinary(tmp);
  deps.replaceSelf(tmp);
  const os = currentServiceOs();
  deps.log(`updated to ${latest}. Restart the services to apply:`);
  deps.log(`  ${restartCoordCmd(os)}`);
  deps.log(`  ${restartWorkerCmd(os)}`);
  return { updated: true, to: latest };
}

export async function update(_args: string[]): Promise<void> {
  if (basename(process.execPath) === "bun") {
    throw new Error("source installs cannot self-update the Bun runtime; install the release binary with install-binary.sh");
  }
  await runUpdate({
    currentVersion: ROOST_VERSION,
    execPath: process.execPath,
    log: (m) => console.log(`>> ${m}`),
    fetchLatestTag: async () => {
      try {
        const r = await fetch(`https://api.github.com/repos/${REPO}/releases/latest`, {
          headers: { accept: "application/vnd.github+json" },
          signal: AbortSignal.timeout(10_000),
        });
        if (!r.ok) return "";
        const j: unknown = await r.json();
        if (j && typeof j === "object" && "tag_name" in j && typeof j.tag_name === "string") {
          return j.tag_name;
        }
        return "";
      } catch {
        return "";
      }
    },
    downloadBinary: (dest) => downloadVerifiedReleaseAsset(releaseAssetName(), dest),
    replaceSelf: (from) => { renameSync(from, process.execPath); },
  });
}
