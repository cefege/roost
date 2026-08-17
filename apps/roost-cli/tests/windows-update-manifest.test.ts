import { describe, expect, test } from "bun:test";
import {
  migrateWindowsCurrentManifestV1,
  parseWindowsCurrentManifest,
  parseWindowsReleaseManifest,
} from "../src/windows-update-journal.ts";

const ARCHIVE_SHA = "a".repeat(64);
const PACKAGE_SHA = "b".repeat(64);
const ROOST_SHA = "c".repeat(64);
const HELPER_SHA = "d".repeat(64);
const SHAWL_EXE_SHA = "e".repeat(64);

describe("Windows release manifest", () => {
  test("keeps the upstream Shawl archive digest distinct from shawl.exe", () => {
    const manifest = parseWindowsReleaseManifest(JSON.stringify({
      schemaVersion: 1,
      version: "0.4.0",
      build: "f".repeat(40),
      platform: "win32",
      arch: "x64",
      publishedAt: "2026-08-16T00:00:00.000Z",
      package: { name: "roost-windows-x64.zip", sha256: PACKAGE_SHA, size: 1024 },
      files: [
        { path: "roost.exe", sha256: ROOST_SHA, size: 512, authenticodeRequired: true },
        { path: "roost-win-helper.exe", sha256: HELPER_SHA, size: 256, authenticodeRequired: true },
        { path: "shawl.exe", sha256: SHAWL_EXE_SHA, size: 128, authenticodeRequired: true },
      ],
      shawl: { version: "1.9.0", upstreamSha256: ARCHIVE_SHA },
    }));

    expect(manifest.shawl.upstreamSha256).toBe(ARCHIVE_SHA);
    expect(manifest.build).toBe("f".repeat(40));
    expect(manifest.files.find((file) => file.path === "shawl.exe")?.sha256).toBe(SHAWL_EXE_SHA);
  });
});

describe("Windows current manifest schemas", () => {
  const legacyCurrent = {
    schemaVersion: 1,
    version: "0.3.0",
    versionDir: "/var/lib/roost/versions/0.3.0",
    files: [{ path: "roost.exe", sha256: ROOST_SHA, size: 512 }],
    manifestUrl: "https://updates.example.test/0.3.0/roost-windows-x64.manifest.json",
    manifestSha256: PACKAGE_SHA,
    publisherSha256: ARCHIVE_SHA,
  } as const;

  test("parses a buildless schema 1 current manifest into a typed legacy form and migrates to v2", () => {
    const parsed = parseWindowsCurrentManifest(JSON.stringify(legacyCurrent));
    expect(parsed.schemaVersion).toBe(1);
    if (parsed.schemaVersion !== 1) throw new Error("expected legacy current manifest");
    expect(parsed.build).toBeUndefined();

    const migrated = migrateWindowsCurrentManifestV1(parsed, "f".repeat(40));
    expect(migrated).toEqual({
      ...legacyCurrent,
      schemaVersion: 2,
      build: "f".repeat(40),
    });
  });

  test("requires valid immutable builds in v2 and validates optional legacy builds when present", () => {
    expect(() => parseWindowsCurrentManifest(JSON.stringify({
      ...legacyCurrent,
      schemaVersion: 2,
    }))).toThrow("current build must be a lowercase immutable build id");

    expect(() => parseWindowsCurrentManifest(JSON.stringify({
      ...legacyCurrent,
      build: "mutable-build",
    }))).toThrow("legacy current build must be a lowercase immutable build id");

    expect(() => parseWindowsCurrentManifest(JSON.stringify({
      ...legacyCurrent,
      schemaVersion: 2,
      build: "f".repeat(40),
      versionDir: "../versions/0.3.0",
    }))).toThrow("current versionDir must be absolute");
  });
});
