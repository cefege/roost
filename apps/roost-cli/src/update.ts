// `roost update` — self-update the single binary from the latest GitHub Release
// (herdr's `update`). Retires the git-pull deploy path for binary installs.
// Pure decision (needsUpdate) + orchestration (runUpdate) are injectable so
// they're unit-tested without a network; the `update` wrapper wires the real
// GitHub fetch + atomic self-replace.
import { createHash, randomUUID, type Hash } from "node:crypto";
import { once } from "node:events";
import { createWriteStream, renameSync } from "node:fs";
import { chmod, rm } from "node:fs/promises";
import { basename } from "node:path";
import { finished } from "node:stream/promises";
import { ROOST_VERSION } from "./version.ts";
import { currentServiceOs, restartCoordCmd, restartWorkerCmd } from "./service-ctl.ts";

const REPO = "cefege/roost";
export const WINDOWS_RELEASE_ASSET = "roost-windows-x64.zip";
export const WINDOWS_RELEASE_MANIFEST_ASSET = "roost-windows-x64.manifest.json";

/** Release asset for a platform/arch pair. `roost` stays unsuffixed for
 *  darwin-arm64 so existing installs' `roost update` keeps resolving; every
 *  other target is explicit. install-binary.sh's `case` mirrors this exactly —
 *  change both together or the installer 404s. */
export function releaseAssetName(platform: string = process.platform, arch: string = process.arch): string {
  if (platform === "darwin" && arch === "arm64") return "roost";
  if (platform === "darwin" && arch === "x64") return "roost-darwin-x64";
  if (platform === "linux" && arch === "x64") return "roost-linux-x64";
  if (platform === "linux" && arch === "arm64") return "roost-linux-arm64";
  if (platform === "win32" && arch === "x64") return WINDOWS_RELEASE_ASSET;
  throw new Error(`no prebuilt roost binary for ${platform}/${arch}`);
}

export interface ReleaseAssetOptions {
  /** Injected by callers that already own a fetch, and by tests. */
  fetchImpl?: typeof fetch;
  /** Overrides the configured mirror and the GitHub origin. */
  releaseBase?: string;
  /** Pin the GitHub origin to one release tag instead of `latest`. A configured
   *  mirror wins: it serves exactly one release directory. */
  tag?: string;
  /** Stream the body to this path (opened 0600, chmod 0755 once verified)
   *  instead of buffering it — a release binary must never be held in memory. */
  destPath?: string;
  /** Names the asset in every failure message; defaults to the asset name. */
  subject?: string;
  /** Wraps every failure. Deploy callers raise DeployFailure, not Error. */
  fail?: (message: string) => Error;
  /** Asset-body timeout. Defaults to the 120s a release binary needs. */
  timeoutMs?: number;
  /** Checksum-sidecar timeout. Defaults to 10s: it is a 65-byte file. */
  checksumTimeoutMs?: number;
}

export interface VerifiedReleaseAsset {
  /** The exact URL the bytes came from. The Windows updater's CMS signature
   *  sidecar is this URL plus `.p7s`. */
  url: string;
  sha256: string;
  bytes: Uint8Array;
}

/** The directory release assets are downloaded from. A mirror is pinned with
 *  ROOST_RELEASE_BASE_URL and this is the CLI's only read of it, so the
 *  self-updater and the fleet deploy paths cannot drift onto different origins
 *  — they had, and only the deploy paths honoured the mirror. */
function releaseBaseUrl(options: ReleaseAssetOptions): string {
  const configured = options.releaseBase ?? process.env.ROOST_RELEASE_BASE_URL;
  if (configured) return configured;
  if (options.tag) {
    return `https://github.com/${REPO}/releases/download/${encodeURIComponent(options.tag)}`;
  }
  return `https://github.com/${REPO}/releases/latest/download`;
}

async function streamToFile(
  body: ReadableStream<Uint8Array>,
  destPath: string,
  hash: Hash,
): Promise<void> {
  const output = createWriteStream(destPath, { flags: "w", mode: 0o600 });
  const reader = body.getReader();
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
}

/** Download one release asset and hand it back only when the sha256 the release
 *  separately published for it matches the bytes actually received. Every
 *  release download in the CLI — self-update, Windows fleet preflight, Windows
 *  coordinator update — resolves through here, so no path can keep a weaker
 *  check or a different origin than its siblings. */
export async function fetchAndVerifyReleaseAsset(
  asset: string,
  options: ReleaseAssetOptions & { destPath: string },
): Promise<Omit<VerifiedReleaseAsset, "bytes">>;
export async function fetchAndVerifyReleaseAsset(
  asset: string,
  options?: ReleaseAssetOptions,
): Promise<VerifiedReleaseAsset>;
export async function fetchAndVerifyReleaseAsset(
  asset: string,
  options: ReleaseAssetOptions = {},
): Promise<VerifiedReleaseAsset | Omit<VerifiedReleaseAsset, "bytes">> {
  const call = options.fetchImpl ?? fetch;
  const subject = options.subject ?? asset;
  const fail = options.fail ?? ((message: string) => new Error(message));
  const url = `${releaseBaseUrl(options)}/${asset}`;
  const destPath = options.destPath;
  // A failed verification must never leave a candidate behind for the atomic
  // self-replace to pick up, and a stale one must not survive the attempt.
  if (destPath !== undefined) await rm(destPath, { force: true });
  try {
    // The checksum lands first so a 404 or a tampered sidecar costs no body
    // transfer, and so nothing is ever written to destPath unverified.
    const checksumResponse = await call(`${url}.sha256`, {
      signal: AbortSignal.timeout(options.checksumTimeoutMs ?? 10_000),
    });
    if (!checksumResponse.ok) {
      throw fail(`${subject} checksum download failed: HTTP ${checksumResponse.status}`);
    }
    // Deliberately wide: the sidecar is REQUIRED, but its formatting proves
    // nothing — the digest is compared byte-for-byte below either way. A mirror
    // that regenerated its sidecars with plain `sha256sum` publishes
    // "<hash>  <filename>" and uppercase hex is equally valid, so rejecting
    // those would make the release path mirror-hostile for no security gain.
    // Do not narrow this. Text that is not a digest at all still fails here.
    const expected = /^([a-f0-9]{64})(?:\s.*)?$/i
      .exec((await checksumResponse.text()).trim())?.[1]
      ?.toLowerCase();
    if (!expected) throw fail(`invalid checksum file for ${subject}`);

    const response = await call(url, { signal: AbortSignal.timeout(options.timeoutMs ?? 120_000) });
    if (!response.ok) throw fail(`${subject} download failed: HTTP ${response.status}`);
    if (!response.body) throw fail(`${subject} download failed: empty response`);

    const hash = createHash("sha256");
    if (destPath !== undefined) {
      await streamToFile(response.body, destPath, hash);
      const sha256 = hash.digest("hex");
      if (sha256 !== expected) throw fail(`checksum mismatch for ${subject}`);
      await chmod(destPath, 0o755);
      return { url, sha256 };
    }
    const bytes = new Uint8Array(await response.arrayBuffer());
    const sha256 = hash.update(bytes).digest("hex");
    if (sha256 !== expected) throw fail(`checksum mismatch for ${subject}`);
    return { url, sha256, bytes };
  } catch (error) {
    if (destPath !== undefined) await rm(destPath, { force: true });
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
  if (process.platform === "win32") {
    if (process.arch !== "x64") throw new Error(`no prebuilt roost binary for win32/${process.arch}`);
    const release = await fetchLatestRelease();
    if (!release.tag) {
      console.log(">> no published release found — nothing to update to.");
      return;
    }
    if (!needsUpdate(ROOST_VERSION, release.tag)) {
      console.log(`>> already up to date (${release.tag}).`);
      return;
    }
    const manifest = await fetchAndVerifyReleaseAsset(WINDOWS_RELEASE_MANIFEST_ASSET, {
      tag: release.tag,
      subject: "Windows release manifest",
      timeoutMs: 30_000,
      checksumTimeoutMs: 30_000,
    });
    const jobId = randomUUID();
    // This Windows-only graph imports native service/durability helpers; keep
    // it out of the POSIX self-replace path, whose load behavior stays intact.
    const { handleUpdateBrokerCommand } = await import("./windows/windows-update-control.ts");
    const progress = await handleUpdateBrokerCommand({
      requestId: randomUUID(),
      jobId,
      action: "START",
      manifestUrl: manifest.url,
      signatureUrl: `${manifest.url}.p7s`,
      manifestSha256: manifest.sha256,
      // The DACL-protected local pin is authoritative. A POSIX coordinator or
      // local CLI is allowed to leave this empty, never to replace that pin.
      publisherSha256: "",
    });
    for (const entry of progress) console.log(`>> [${entry.phase}] ${entry.message}`);
    console.log(`>> update ${jobId} staged; RoostUpdaterV2 was started through SCM.`);
    return;
  }
  if (process.platform !== "darwin" && process.platform !== "linux") {
    throw new Error(`self-update is unsupported on ${process.platform}`);
  }
  await runUpdate({
    currentVersion: ROOST_VERSION,
    execPath: process.execPath,
    log: (m) => console.log(`>> ${m}`),
    fetchLatestTag: async () => (await fetchLatestRelease()).tag,
    downloadBinary: async (dest) => {
      await fetchAndVerifyReleaseAsset(releaseAssetName(), { destPath: dest });
    },
    replaceSelf: (from) => { renameSync(from, process.execPath); },
  });
}

async function fetchLatestRelease(): Promise<{ tag: string }> {
  try {
    const response = await fetch(`https://api.github.com/repos/${REPO}/releases/latest`, {
      headers: { accept: "application/vnd.github+json" },
      signal: AbortSignal.timeout(10_000),
    });
    if (!response.ok) return { tag: "" };
    const value: unknown = await response.json();
    if (value && typeof value === "object" && "tag_name" in value && typeof value.tag_name === "string") {
      return { tag: value.tag_name };
    }
    return { tag: "" };
  } catch {
    return { tag: "" };
  }
}
