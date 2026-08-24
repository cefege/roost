// Signed Windows release-manifest and current.json schemas plus their
// parsers: the CMS-covered release manifest (v1), the current-version
// pointer (schemas 1 and 2), and the authenticated v1→v2 build migration.
//
// Callers: windows-update-journal.ts (re-exports these for the broker,
// control plane, and tests), windows-update-broker.ts, and
// windows-update-control.ts. Scalar validation comes from
// windows-journal-validate.ts.

import {
  absolutePath,
  buildIdentity,
  fileSize,
  httpsUrl,
  isoTimestamp,
  nonempty,
  parseJson,
  record,
  releaseVersion,
  safeRelative,
  sha,
  SHA256_RE,
} from "./windows-journal-validate.ts";

export const WINDOWS_CURRENT_MANIFEST_SCHEMA_VERSION = 2 as const;

export interface WindowsReleaseFile {
  path: string;
  sha256: string;
  size: number;
  authenticodeRequired: boolean;
}

/** Detached CMS covers the exact raw bytes containing this JSON. */
export interface WindowsReleaseManifestV1 {
  schemaVersion: 1;
  version: string;
  build: string;
  platform: "win32";
  arch: "x64";
  publishedAt: string;
  package: { name: "roost-windows-x64.zip"; sha256: string; size: number };
  files: WindowsReleaseFile[];
  shawl: { version: "1.9.0"; upstreamSha256: string };
}

interface WindowsCurrentManifestFields {
  version: string;
  versionDir: string;
  files: Array<Pick<WindowsReleaseFile, "path" | "sha256" | "size">>;
  manifestUrl: string;
  manifestSha256: string;
  publisherSha256: string;
}

/** Legacy current.json did not contain an immutable build identity. */
export interface WindowsCurrentManifestV1 extends WindowsCurrentManifestFields {
  schemaVersion: 1;
  build?: string;
}

export interface WindowsCurrentManifestV2 extends WindowsCurrentManifestFields {
  schemaVersion: 2;
  build: string;
}

export type WindowsCurrentManifest = WindowsCurrentManifestV1 | WindowsCurrentManifestV2;

export function parseWindowsBuildIdentity(value: unknown, label = "build"): string {
  return buildIdentity(value, label);
}

export function parseWindowsReleaseManifest(raw: string | Uint8Array): WindowsReleaseManifestV1 {
  const o = record(
    parseJson(typeof raw === "string" ? raw : Buffer.from(raw).toString("utf8"), "Windows release manifest"),
    "Windows release manifest",
  );
  if (o.schemaVersion !== 1) {
    throw new Error(`unsupported Windows release manifest schema: ${String(o.schemaVersion)}`);
  }
  if (o.platform !== "win32" || o.arch !== "x64") {
    throw new Error(`manifest targets ${String(o.platform)}/${String(o.arch)}, expected win32/x64`);
  }
  const version = releaseVersion(o.version, "manifest.version");
  const build = buildIdentity(o.build, "manifest.build");
  const publishedAt = isoTimestamp(o.publishedAt, "manifest.publishedAt");
  const pkg = record(o.package, "manifest.package");
  if (pkg.name !== "roost-windows-x64.zip") throw new Error("unexpected Windows package name");
  if (!Array.isArray(o.files) || o.files.length === 0) throw new Error("manifest.files must be non-empty");
  const seen = new Set<string>();
  const files = o.files.map((value, index): WindowsReleaseFile => {
    const f = record(value, `manifest.files[${index}]`);
    const path = safeRelative(f.path, `manifest.files[${index}].path`);
    const folded = path.toLowerCase();
    if (seen.has(folded)) throw new Error(`duplicate manifest asset: ${path}`);
    seen.add(folded);
    if (typeof f.authenticodeRequired !== "boolean") {
      throw new Error(`manifest.files[${index}].authenticodeRequired is invalid`);
    }
    return {
      path,
      sha256: sha(f.sha256, `manifest.files[${index}].sha256`),
      size: fileSize(f.size, `manifest.files[${index}].size`),
      authenticodeRequired: f.authenticodeRequired,
    };
  });
  for (const required of ["roost.exe", "roost-win-helper.exe", "shawl.exe"]) {
    const asset = files.find(({ path }) => path.toLowerCase() === required);
    if (!asset) throw new Error(`manifest is missing ${required}`);
    if (!asset.authenticodeRequired) throw new Error(`manifest must require Authenticode for ${required}`);
  }
  const shawl = record(o.shawl, "manifest.shawl");
  if (shawl.version !== "1.9.0") {
    throw new Error(`unsupported Shawl version: ${String(shawl.version)}`);
  }
  // The upstream digest pins the Shawl release archive; it is not the digest
  // of the extracted, separately Authenticode-verified shawl.exe.
  const upstreamSha256 = sha(shawl.upstreamSha256, "manifest.shawl.upstreamSha256");
  return {
    schemaVersion: 1,
    version,
    build,
    platform: "win32",
    arch: "x64",
    publishedAt,
    package: {
      name: "roost-windows-x64.zip",
      sha256: sha(pkg.sha256, "manifest.package.sha256"),
      size: fileSize(pkg.size, "manifest.package.size"),
    },
    files,
    shawl: { version: "1.9.0", upstreamSha256 },
  };
}

export function parseWindowsCurrentManifest(raw: string): WindowsCurrentManifest {
  const o = record(parseJson(raw, "Windows current manifest"), "Windows current manifest");
  if (o.schemaVersion !== 1 && o.schemaVersion !== WINDOWS_CURRENT_MANIFEST_SCHEMA_VERSION) {
    throw new Error(`unsupported Windows current manifest schema: ${String(o.schemaVersion)}`);
  }
  if (!Array.isArray(o.files)) throw new Error("current manifest files must be an array");
  const files = parseCurrentFiles(o.files);
  const fields: WindowsCurrentManifestFields = {
    version: releaseVersion(o.version, "current version"),
    versionDir: absolutePath(o.versionDir, "current versionDir"),
    files,
    manifestUrl: httpsUrl(o.manifestUrl, "current manifestUrl"),
    manifestSha256: sha(o.manifestSha256, "current manifestSha256"),
    publisherSha256: normalizedSha(o.publisherSha256, "current publisherSha256"),
  };
  if (o.schemaVersion === 1) {
    const build = Object.hasOwn(o, "build")
      ? buildIdentity(o.build, "legacy current build")
      : undefined;
    return { schemaVersion: 1, ...fields, ...(build ? { build } : {}) };
  }
  return {
    schemaVersion: WINDOWS_CURRENT_MANIFEST_SCHEMA_VERSION,
    ...fields,
    build: buildIdentity(o.build, "current build"),
  };
}

export function migrateWindowsCurrentManifestV1(
  current: WindowsCurrentManifestV1,
  authenticatedBuild: string,
): WindowsCurrentManifestV2 {
  const build = buildIdentity(authenticatedBuild, "authenticated current build");
  if (current.build && current.build !== build) {
    throw new Error("legacy current manifest build disagrees with authenticated running build");
  }
  const migrated: WindowsCurrentManifestV2 = {
    ...current,
    schemaVersion: WINDOWS_CURRENT_MANIFEST_SCHEMA_VERSION,
    build,
  };
  const parsed = parseWindowsCurrentManifest(JSON.stringify(migrated));
  if (parsed.schemaVersion !== 2) throw new Error("current manifest migration did not produce schema 2");
  return parsed;
}

function normalizedSha(value: unknown, label: string): string {
  const normalized = typeof value === "string" ? value.trim().toLowerCase() : "";
  if (!SHA256_RE.test(normalized)) throw new Error(`${label} must be SHA-256`);
  return normalized;
}

function parseCurrentFiles(values: unknown[]): Array<Pick<WindowsReleaseFile, "path" | "sha256" | "size">> {
  const seen = new Set<string>();
  return values.map((value, index) => {
    const file = record(value, `current files[${index}]`);
    const path = safeRelative(file.path, `current files[${index}].path`);
    if (seen.has(path.toLowerCase())) throw new Error(`duplicate current manifest asset: ${path}`);
    seen.add(path.toLowerCase());
    return {
      path,
      sha256: sha(file.sha256, `current files[${index}].sha256`),
      size: fileSize(file.size, `current files[${index}].size`),
    };
  });
}
