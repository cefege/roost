// Windows coordinator state preparation owns canonical paths and legacy migration.
// Quickstart calls it inside the machine transaction before mutating services.
// Its journal and security snapshots make the directory move exactly reversible.

import { randomUUID } from "node:crypto";
import {
  existsSync,
  lstatSync,
  mkdirSync,
  renameSync,
  rmdirSync,
  unlinkSync,
} from "node:fs";
import { dirname, join, win32 } from "node:path";
import {
  durableRemove,
  durableReplace,
  durableWriteFile,
  flushDurablePath,
} from "@roost/shared/durability";
import { coordDataDir } from "@roost/shared/paths";
import { windowsApplyArtifactDacl } from "@roost/shared/windows-helper";
import {
  protectWindowsRoleStateTree,
  restoreWindowsFileSecurityTree,
  snapshotWindowsFileSecurityTree,
  type WindowsFileSecurityTreeSnapshot,
} from "./install-binary-agents.ts";

export interface CoordinatorPaths {
  dataDir: string;
  logDir: string;
  tlsDir: string;
  database: string;
  authorizedKeys: string;
  key: string;
  handoff: string;
}

export function requireCanonicalWindowsPath(name: string, expected?: string): string {
  const value = process.env[name]?.trim();
  if (!value || !win32.isAbsolute(value) || /[\0\r\n]/.test(value)) {
    throw new Error(`${name} must be an explicit absolute Windows path`);
  }
  const normalized = win32.normalize(value);
  const canonical = normalized.length > 3 ? normalized.replace(/[\\/]+$/, "") : normalized;
  if (expected) {
    const normalizedExpected = win32.normalize(expected);
    const canonicalExpected = normalizedExpected.length > 3
      ? normalizedExpected.replace(/[\\/]+$/, "")
      : normalizedExpected;
    if (canonical.toLocaleLowerCase("en-US") !== canonicalExpected.toLocaleLowerCase("en-US")) {
      throw new Error(`${name} does not match the canonical Windows install layout`);
    }
  }
  return canonical;
}

export function coordinatorPaths(): CoordinatorPaths {
  if (process.platform !== "win32") {
    const dataDir = coordDataDir();
    return {
      dataDir,
      logDir: process.env.ROOST_COORD_LOG_DIR ?? join(dataDir, "..", "..", "logs", "coordinator"),
      tlsDir: process.env.ROOST_COORDINATOR_TLS_DIR ?? join(dataDir, "tls"),
      database: process.env.ROOST_COORDINATOR_DB ?? join(dataDir, "coordinator_v2.db"),
      authorizedKeys: process.env.ROOST_COORDINATOR_AUTHORIZED_KEYS ?? join(dataDir, "authorized_keys.roost"),
      key: process.env.ROOST_COORDINATOR_KEY_PATH ?? join(dataDir, "ssh_ed25519.key"),
      handoff: process.env.ROOST_COORDINATOR_HANDOFF_PATH ?? join(dataDir, "coord-handoff.json"),
    };
  }
  const installRoot = requireCanonicalWindowsPath("ROOST_INSTALL_ROOT");
  const serviceDir = requireCanonicalWindowsPath(
    "ROOST_SERVICE_DIR",
    win32.join(installRoot, "service"),
  );
  const dataDir = requireCanonicalWindowsPath(
    "ROOST_COORD_DATA_DIR",
    win32.join(serviceDir, "data", "coordinator"),
  );
  return {
    dataDir,
    logDir: requireCanonicalWindowsPath(
      "ROOST_COORD_LOG_DIR",
      win32.join(serviceDir, "logs", "coordinator"),
    ),
    tlsDir: requireCanonicalWindowsPath(
      "ROOST_COORDINATOR_TLS_DIR",
      win32.join(dataDir, "tls"),
    ),
    database: requireCanonicalWindowsPath(
      "ROOST_COORDINATOR_DB",
      win32.join(dataDir, "coordinator_v2.db"),
    ),
    authorizedKeys: requireCanonicalWindowsPath(
      "ROOST_COORDINATOR_AUTHORIZED_KEYS",
      win32.join(dataDir, "authorized_keys.roost"),
    ),
    key: requireCanonicalWindowsPath(
      "ROOST_COORDINATOR_KEY_PATH",
      win32.join(dataDir, "ssh_ed25519.key"),
    ),
    handoff: requireCanonicalWindowsPath(
      "ROOST_COORDINATOR_HANDOFF_PATH",
      win32.join(dataDir, "coord-handoff.json"),
    ),
  };
}

export interface WindowsLegacyCoordinatorMigration {
  schemaVersion: 1;
  phase: "prepared" | "moved" | "hardened";
  legacyPath: string;
  canonicalPath: string;
  journalPath: string;
  helperPath: string;
  security: WindowsFileSecurityTreeSnapshot;
}

async function persistLegacyCoordinatorMigration(
  migration: WindowsLegacyCoordinatorMigration,
): Promise<void> {
  const temporary = `${migration.journalPath}.${process.pid}.${randomUUID()}.tmp`;
  try {
    await durableWriteFile(temporary, `${JSON.stringify(migration)}\n`, { platform: "win32" });
    await windowsApplyArtifactDacl(temporary, "NT SERVICE\\RoostUpdaterV2", {
      helperPath: migration.helperPath,
    });
    await durableReplace(temporary, migration.journalPath, { platform: "win32" });
    await windowsApplyArtifactDacl(migration.journalPath, "NT SERVICE\\RoostUpdaterV2", {
      helperPath: migration.helperPath,
    });
    await flushDurablePath(dirname(migration.journalPath), { platform: "win32" });
  } finally {
    await durableRemove(temporary, { platform: "win32" }).catch(() => undefined);
  }
}

async function removeLegacyMigrationAdditions(
  migration: WindowsLegacyCoordinatorMigration,
): Promise<void> {
  const current = await snapshotWindowsFileSecurityTree(migration.legacyPath, {
    helperPath: migration.helperPath,
  });
  const original = new Set(migration.security.entries.map((entry) =>
    entry.relativePath.replace(/\//g, "\\").toLocaleLowerCase("en-US")
  ));
  const additions = current.entries
    .filter((entry) =>
      entry.relativePath !== ""
      && !original.has(entry.relativePath.replace(/\//g, "\\").toLocaleLowerCase("en-US"))
    )
    .sort((left, right) =>
      right.relativePath.split(/[\\/]/).length - left.relativePath.split(/[\\/]/).length
    );
  for (const entry of additions) {
    const path = win32.join(migration.legacyPath, entry.relativePath);
    if (entry.kind === "file") unlinkSync(path);
    else rmdirSync(path);
  }
}

export async function rollbackLegacyCoordinatorMigration(
  migration: WindowsLegacyCoordinatorMigration,
): Promise<void> {
  if (existsSync(migration.canonicalPath)) {
    if (existsSync(migration.legacyPath)) {
      throw new Error("cannot roll back legacy coordinator state because both roots exist");
    }
    renameSync(migration.canonicalPath, migration.legacyPath);
  }
  if (!existsSync(migration.legacyPath)) {
    throw new Error("cannot roll back legacy coordinator state because both roots are missing");
  }
  await removeLegacyMigrationAdditions(migration);
  await restoreWindowsFileSecurityTree(
    { ...migration.security, root: migration.legacyPath },
    { helperPath: migration.helperPath },
  );
  await durableRemove(migration.journalPath, { platform: "win32" });
  await flushDurablePath(dirname(migration.journalPath), { platform: "win32" });
}

export async function prepareWindowsCoordinatorState(
  paths: CoordinatorPaths,
  account: string,
  journalPath: string,
): Promise<WindowsLegacyCoordinatorMigration | null> {
  const legacyPath = requireCanonicalWindowsPath("ROOST_LEGACY_COORD_DATA_DIR");
  if (existsSync(journalPath)) {
    const journalInfo = lstatSync(journalPath);
    if (!journalInfo.isFile() || journalInfo.isSymbolicLink()) {
      throw new Error("legacy coordinator migration journal is not a regular non-reparse file");
    }
    throw new Error(
      `incomplete legacy coordinator migration journal requires recovery before SCM mutation: ${journalPath}`,
    );
  }
  const helperPath = requireCanonicalWindowsPath("ROOST_WIN_HELPER");
  const interactiveSid = process.env.ROOST_INTERACTIVE_SID?.trim() ?? "";
  if (!/^S-1-(?:\d+-)+\d+$/.test(interactiveSid)) {
    throw new Error("ROOST_INTERACTIVE_SID is required for coordinator state migration");
  }
  if (legacyPath.toLocaleLowerCase("en-US") === paths.dataDir.toLocaleLowerCase("en-US")) {
    throw new Error("legacy and canonical coordinator data roots must differ");
  }
  mkdirSync(dirname(paths.dataDir), { recursive: true });
  if (!existsSync(legacyPath)) {
    if (!existsSync(paths.dataDir)) {
      mkdirSync(paths.dataDir, { recursive: false });
    }
    await protectWindowsRoleStateTree(paths.dataDir, "coordinator-state", {
      account,
      interactiveSid,
      helperPath,
    });
    return null;
  }
  if (existsSync(paths.dataDir)) {
    throw new Error("refusing to merge legacy and canonical coordinator data roots");
  }
  const security = await snapshotWindowsFileSecurityTree(legacyPath, { helperPath });
  let migration: WindowsLegacyCoordinatorMigration = {
    schemaVersion: 1,
    phase: "prepared",
    legacyPath,
    canonicalPath: paths.dataDir,
    journalPath,
    helperPath,
    security,
  };
  await persistLegacyCoordinatorMigration(migration);
  try {
    renameSync(legacyPath, paths.dataDir);
    migration = { ...migration, phase: "moved" };
    await persistLegacyCoordinatorMigration(migration);
    await protectWindowsRoleStateTree(paths.dataDir, "coordinator-state", {
      account,
      interactiveSid,
      helperPath,
    });
    migration = { ...migration, phase: "hardened" };
    await persistLegacyCoordinatorMigration(migration);
    return migration;
  } catch (error) {
    try {
      await rollbackLegacyCoordinatorMigration(migration);
    } catch (rollbackError) {
      throw new Error(`${String(error)}; legacy coordinator migration rollback failed: ${String(rollbackError)}`);
    }
    throw error;
  }
}

export async function commitLegacyCoordinatorMigration(
  migration: WindowsLegacyCoordinatorMigration | null,
): Promise<void> {
  if (!migration) return;
  await durableRemove(migration.journalPath, { platform: "win32" });
  await flushDurablePath(dirname(migration.journalPath), { platform: "win32" });
}
