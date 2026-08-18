// Release-manifest preflight for the signed Windows update path: resolve the
// release base, fetch the manifest plus its sha256 sidecar, digest-verify them
// against each other and against the fleet's expected build/manifest identity,
// then hand the verified URLs to startWindowsUpdateDeploy. Split out of
// connect/handlers-workers.ts (400-line cap) — fetching and digest-verifying a
// GitHub release manifest is deploy orchestration, not RPC marshalling.

import { createHash } from "node:crypto";

import type { DeployStartResult } from "./deploy-jobs.ts";
import { startWindowsUpdateDeploy } from "./windows-update-deploy-jobs.ts";
import { listRoutableFps } from "./connect/worker-service.ts";

export async function startWindowsDeploy(
  workerFp: string,
  expectedGitSha?: string,
  expectedManifestSha256?: string,
): Promise<DeployStartResult> {
  const publisherSha256 = process.env.ROOST_WINDOWS_PUBLISHER_SHA256 ?? "";
  if (!listRoutableFps().includes(workerFp)) {
    return { ok: false, error: "Windows worker is offline; signed update control requires its authenticated worker link" };
  }
  if (expectedGitSha !== undefined && !/^[a-f0-9]{40,64}$/i.test(expectedGitSha)) {
    return { ok: false, error: "expected Windows build identity is malformed" };
  }
  if (expectedManifestSha256 !== undefined && !/^[a-f0-9]{64}$/i.test(expectedManifestSha256)) {
    return { ok: false, error: "expected Windows manifest digest is malformed" };
  }
  const releaseBase = process.env.ROOST_RELEASE_BASE_URL
    ?? "https://github.com/cefege/roost/releases/latest/download";
  const manifestUrl = `${releaseBase}/roost-windows-x64.manifest.json`;
  const signatureUrl = `${releaseBase}/roost-windows-x64.manifest.json.p7s`;
  let manifestSha256 = "";
  try {
    const [manifestResponse, checksumResponse] = await Promise.all([
      fetch(manifestUrl, { signal: AbortSignal.timeout(10_000) }),
      fetch(`${manifestUrl}.sha256`, { signal: AbortSignal.timeout(10_000) }),
    ]);
    if (!manifestResponse.ok) {
      return { ok: false, error: `Windows manifest download failed: HTTP ${manifestResponse.status}` };
    }
    if (!checksumResponse.ok) {
      return { ok: false, error: `Windows manifest checksum download failed: HTTP ${checksumResponse.status}` };
    }
    const manifestBytes = new Uint8Array(await manifestResponse.arrayBuffer());
    manifestSha256 = createHash("sha256").update(manifestBytes).digest("hex");
    const match = /^([a-f0-9]{64})(?:\s.*)?$/i.exec((await checksumResponse.text()).trim());
    if (!match || match[1]!.toLowerCase() !== manifestSha256) {
      return { ok: false, error: "Windows manifest checksum does not match the release manifest" };
    }
    if (expectedManifestSha256
      && expectedManifestSha256.toLowerCase() !== manifestSha256) {
      return { ok: false, error: "Windows release manifest changed after fleet preflight" };
    }
    let manifest: unknown;
    try {
      manifest = JSON.parse(new TextDecoder().decode(manifestBytes));
    } catch {
      return { ok: false, error: "Windows release manifest is not valid JSON" };
    }
    if (!manifest
      || typeof manifest !== "object"
      || !("schemaVersion" in manifest)
      || manifest.schemaVersion !== 1
      || !("platform" in manifest)
      || manifest.platform !== "win32"
      || !("arch" in manifest)
      || manifest.arch !== "x64"
      || !("version" in manifest)
      || typeof manifest.version !== "string"
      || !/^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?$/.test(manifest.version)
      || !("build" in manifest)
      || typeof manifest.build !== "string"
      || !/^[a-f0-9]{40,64}$/i.test(manifest.build)) {
      return { ok: false, error: "Windows release manifest identity is malformed" };
    }
    const build = manifest.build;
    if (expectedGitSha && build.toLowerCase() !== expectedGitSha.toLowerCase()) {
      return {
        ok: false,
        error: `Windows release build ${build} does not match requested fleet build ${expectedGitSha}`,
      };
    }
  } catch (error) {
    return { ok: false, error: `Windows manifest preflight failed: ${String(error)}` };
  }
  return await startWindowsUpdateDeploy(workerFp, {
    workerFp,
    manifestUrl,
    signatureUrl,
    manifestSha256,
    publisherSha256,
  });
}
