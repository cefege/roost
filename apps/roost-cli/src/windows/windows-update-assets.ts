// Staged-release asset verification for the Windows update broker: held-handle
// transaction primitives (copy/inspect through the native helper), signed
// manifest + package verification, zip extraction with exact-tree proofs, and
// small fs helpers shared by the forward path and rollback.
//
// Callers: windows-update-broker.ts (forward staging + cleanup proofs),
// windows-update-rollback.ts, windows-update-stable-artifacts.ts.
// Depends on windows-update-journal.ts and windows-path-safety.ts.

import { createHash, randomUUID } from "node:crypto";
import { createReadStream } from "node:fs";
import { lstat, mkdir, readFile, readdir, rm } from "node:fs/promises";
import { basename, dirname, join } from "node:path";
import {
  durableReplace,
  flushDurablePath,
} from "@roost/shared/durability";
import {
  parseWindowsReleaseManifest,
  replaceWindowsUpdaterArtifact,
  sha256Hex,
  type WindowsReleaseFile,
  type WindowsUpdateJournalV2,
  type WindowsUpdaterPersistenceProfile,
} from "./windows-update-journal.ts";
import { nodeError, resolveUnder } from "./windows-path-safety.ts";
import type { WindowsUpdateNative } from "./windows-update-broker.ts";

export function assertPromotionEvidence(
  journal: WindowsUpdateJournalV2,
): asserts journal is WindowsUpdateJournalV2 & {
  signedManifest: WindowsUpdateJournalV2["signedManifest"] & {
    path: string;
    signaturePath: string;
  };
  releasePackage: NonNullable<WindowsUpdateJournalV2["releasePackage"]>;
} {
  if (
    journal.stableArtifacts.mode !== "promote"
    || journal.signedManifest.path === null
    || journal.signedManifest.signaturePath === null
    || journal.releasePackage === null
  ) {
    throw new Error("promotion journal lacks signed staged evidence");
  }
}

export function nativeTransactionOps(native: WindowsUpdateNative): {
  copyArtifact(
    sourcePath: string,
    destinationPath: string,
    sourceProfile: WindowsUpdaterPersistenceProfile,
    destinationProfile: WindowsUpdaterPersistenceProfile,
    expected?: { sha256: string; size: number },
  ): Promise<{ sha256: string; size: number; sddl: string }>;
  inspectArtifact(
    path: string,
    profile: WindowsUpdaterPersistenceProfile,
    expected?: { sha256: string; size: number },
  ): Promise<{ sha256: string; size: number; sddl: string }>;
} {
  if (native.copyArtifact && native.inspectArtifact) {
    return {
      copyArtifact: native.copyArtifact.bind(native),
      inspectArtifact: native.inspectArtifact.bind(native),
    };
  }
  if (process.platform === "win32") {
    throw new Error("Windows updater native held-handle transaction operations are unavailable");
  }
  const inspect = async (
    path: string,
    _profile: WindowsUpdaterPersistenceProfile,
    expected?: { sha256: string; size: number },
  ): Promise<{ sha256: string; size: number; sddl: string }> => {
    const bytes = await readFile(path);
    const actual = {
      sha256: sha256Hex(bytes),
      size: bytes.byteLength,
      sddl: "non-windows-test-security-descriptor",
    };
    if (expected && (actual.sha256 !== expected.sha256 || actual.size !== expected.size)) {
      throw new Error(`artifact source does not match expected identity: ${path}`);
    }
    return actual;
  };
  return {
    inspectArtifact: inspect,
    copyArtifact: async (sourcePath, destinationPath, sourceProfile, destinationProfile, expected) => {
      const actual = await inspect(sourcePath, sourceProfile, expected);
      await replaceWindowsUpdaterArtifact(destinationPath, await readFile(sourcePath), destinationProfile);
      return actual;
    },
  };
}

export async function stageAndVerifyAssets(
  journal: WindowsUpdateJournalV2,
  native: WindowsUpdateNative,
): Promise<void> {
  assertPromotionEvidence(journal);
  const raw = await readFile(journal.signedManifest.path);
  if (sha256Hex(raw) !== journal.signedManifest.sha256) {
    throw new Error("staged manifest digest changed");
  }
  await native.verifyCmsDetached(
    journal.signedManifest.path,
    journal.signedManifest.signaturePath,
    journal.signedManifest.publisherSha256,
  );
  const manifest = parseWindowsReleaseManifest(raw);
  if (
    manifest.version !== journal.targetVersion
    || manifest.build !== journal.targetBuild
    || manifest.package.sha256 !== journal.releasePackage.sha256
    || manifest.package.size !== journal.releasePackage.size
    || JSON.stringify(manifest.files) !== JSON.stringify(journal.assets)
  ) {
    throw new Error("journal does not match signed manifest");
  }
  await verifyFile(journal.releasePackage.path, journal.releasePackage.sha256, journal.releasePackage.size);
  if (await directoryExists(journal.paths.newVersionDir)) {
    await verifyTree(
      journal.paths.newVersionDir,
      journal.assets,
      journal.signedManifest.publisherSha256,
      native,
    );
    return;
  }
  const versionParent = dirname(journal.paths.newVersionDir);
  await mkdir(versionParent, { recursive: true });
  const extracted = join(
    versionParent,
    `.extracting-${basename(journal.paths.newVersionDir)}-${process.pid}-${randomUUID()}`,
  );
  let installed = false;
  try {
    await native.extractZip(journal.releasePackage.path, extracted, journal.assets);
    await verifyTree(extracted, journal.assets, journal.signedManifest.publisherSha256, native);
    await durableReplace(extracted, journal.paths.newVersionDir);
    installed = true;
  } finally {
    if (!installed) await rm(extracted, { recursive: true, force: true }).catch(() => undefined);
  }
  await flushDurablePath(versionParent);
  await verifyTree(
    journal.paths.newVersionDir,
    journal.assets,
    journal.signedManifest.publisherSha256,
    native,
  );
}

export async function verifyTree(
  root: string,
  assets: readonly WindowsReleaseFile[],
  publisher: string,
  native: WindowsUpdateNative,
): Promise<void> {
  const expected = new Set(assets.map((asset) => asset.path.replaceAll("\\", "/").toLowerCase()));
  const actual = await listFiles(root);
  for (const path of actual) {
    if (!expected.has(path.toLowerCase())) throw new Error(`unmanifested archive asset: ${path}`);
  }
  if (actual.length !== expected.size) throw new Error("archive asset count mismatch");
  const held = nativeTransactionOps(native);
  for (const asset of assets) {
    const path = resolveUnder(root, asset.path);
    await held.inspectArtifact(
      path,
      "release",
      { sha256: asset.sha256, size: asset.size },
    );
    if (asset.authenticodeRequired) await native.verifyAuthenticode(path, publisher);
  }
}

export async function verifyFile(path: string, expected: string, bytes: number): Promise<void> {
  const info = await lstat(path);
  if (!info.isFile() || info.isSymbolicLink() || info.size !== bytes) {
    throw new Error(`asset metadata mismatch: ${path}`);
  }
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk as Buffer);
  if (hash.digest("hex") !== expected) throw new Error(`asset digest mismatch: ${path}`);
}

async function listFiles(root: string, prefix = ""): Promise<string[]> {
  const result: string[] = [];
  for (const entry of await readdir(join(root, prefix), { withFileTypes: true })) {
    const path = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isSymbolicLink()) throw new Error(`archive link/reparse asset: ${path}`);
    if (entry.isDirectory()) result.push(...await listFiles(root, path));
    else if (entry.isFile()) result.push(path);
    else throw new Error(`unsupported archive asset: ${path}`);
  }
  return result;
}

export async function directoryExists(path: string): Promise<boolean> {
  try {
    return (await lstat(path)).isDirectory();
  } catch (error) {
    if (nodeError(error)?.code === "ENOENT") return false;
    throw error;
  }
}
