// Windows quickstart installation owns the machine transaction and exact rollback.
// The command orchestrator delegates service snapshots, state/TLS restoration,
// and post-install identity proof so every failure leaves one auditable boundary.

import { lstatSync } from "node:fs";
import { dirname, join, win32 } from "node:path";
import { ROOST_BUILD_SHA } from "@roost/shared/build-identity";
import { roostServiceDir } from "@roost/shared/paths";
import { probeServiceHealth } from "@roost/shared/service-health";
import {
  acquireMachineTransaction,
  type MachineTransactionLock,
} from "./machine-transaction.ts";
import {
  WINDOWS_SERVICE_ROLES,
  createWindowsServiceManager,
  type WindowsServiceManager,
  type WindowsServiceSnapshotSet,
} from "./service-ctl.ts";
import { ROOST_VERSION } from "./version.ts";
import type {
  ResolvedQuickstartEndpoint,
} from "./quickstart-endpoint.ts";
import {
  commitLegacyCoordinatorMigration,
  prepareWindowsCoordinatorState,
  rollbackLegacyCoordinatorMigration,
  type CoordinatorPaths,
  type WindowsLegacyCoordinatorMigration,
} from "./quickstart-windows-state.ts";
import {
  commitWindowsTlsInstall,
  prepareWindowsTlsInstall,
  rollbackWindowsTlsInstall,
  type WindowsTlsInstallRollback,
} from "./quickstart-windows-tls.ts";

export interface WindowsQuickstartInstall {
  lock: MachineTransactionLock;
  manager: WindowsServiceManager;
  snapshot: WindowsServiceSnapshotSet;
  committed: boolean;
  migration: WindowsLegacyCoordinatorMigration | null;
  migrationJournalPath: string;
  tls: WindowsTlsInstallRollback | null;
}

export async function beginWindowsQuickstartInstall(): Promise<WindowsQuickstartInstall> {
  const serviceDir = roostServiceDir(undefined, "win32");
  const transactionJournalPath = join(serviceDir, "install-transaction.json");
  const migrationJournalPath = join(serviceDir, "coordinator-migration.json");
  const lock = await acquireMachineTransaction(
    "install",
    transactionJournalPath,
    { platform: "win32" },
  );
  const manager = createWindowsServiceManager();
  try {
    return {
      lock,
      manager,
      snapshot: await manager.snapshot({ includeSecurity: true }),
      committed: false,
      migration: null,
      migrationJournalPath,
      tls: null,
    };
  } catch (error) {
    await lock.release();
    throw error;
  }
}

export async function prepareWindowsQuickstartCoordinatorState(
  installation: WindowsQuickstartInstall,
  paths: CoordinatorPaths,
  account: string,
): Promise<void> {
  installation.migration = await prepareWindowsCoordinatorState(
    paths,
    account,
    installation.migrationJournalPath,
  );
}

export async function prepareWindowsQuickstartTls(
  installation: WindowsQuickstartInstall,
  endpoint: ResolvedQuickstartEndpoint,
  paths: CoordinatorPaths,
  account: string,
  interactiveSid: string,
): Promise<ResolvedQuickstartEndpoint> {
  const copied = await prepareWindowsTlsInstall(
    endpoint,
    paths,
    account,
    interactiveSid,
  );
  installation.tls = copied.state;
  return {
    ...endpoint,
    tlsCertPath: copied.certPath,
    tlsKeyPath: copied.keyPath,
  };
}

export async function proveWindowsInstallHealth(
  installation: WindowsQuickstartInstall,
  expectedAccount: string,
  expectedCoordinatorUrl: string,
): Promise<void> {
  const snapshots = await installation.manager.snapshot();
  for (const role of ["keeper", "worker", "coordinator"] as const) {
    const service = snapshots[role];
    if (!service.installed || service.state !== "running") {
      throw new Error(`${service.name} did not reach the required running state`);
    }
    if (service.account?.trim().toLowerCase() !== expectedAccount.trim().toLowerCase()) {
      throw new Error(`${service.name} does not use the dedicated service account`);
    }
  }
  const updater = snapshots.updater;
  if (
    !updater.installed
    || updater.startMode !== "automatic"
    || updater.account?.trim().toLowerCase() !== expectedAccount.trim().toLowerCase()
  ) {
    throw new Error("RoostUpdaterV2 automatic recovery/account proof failed");
  }
  await Promise.all([
    probeServiceHealth("coordinator", {
      expectedVersion: ROOST_VERSION,
      expectedBuild: ROOST_BUILD_SHA,
    }),
    probeServiceHealth("worker", {
      expectedVersion: ROOST_VERSION,
      expectedBuild: ROOST_BUILD_SHA,
      expectedCoordinatorUrl,
    }),
  ]);
  const launcher = process.env.ROOST_STABLE_LAUNCHER?.trim();
  if (!launcher || !win32.isAbsolute(launcher) || /[\0\r\n]/.test(launcher)) {
    throw new Error("Windows stable launcher path was not provided by the signed installer");
  }
  const launcherInfo = lstatSync(launcher);
  if (!launcherInfo.isFile() || launcherInfo.isSymbolicLink()) {
    throw new Error("Windows stable launcher is not a non-reparse regular file");
  }
  const version = Bun.spawnSync([launcher, "version"]);
  if (version.exitCode !== 0 || version.stdout.toString().trim() !== ROOST_VERSION) {
    throw new Error("Windows stable launcher did not execute the selected Roost version");
  }
  const stableBin = dirname(launcher).replace(/[\\/]+$/, "").toLowerCase();
  const pathSegments = (process.env.Path ?? process.env.PATH ?? "")
    .split(";")
    .map((segment) => segment.replace(/[\\/]+$/, "").toLowerCase());
  if (!pathSegments.includes(stableBin)) {
    throw new Error("Windows stable launcher directory is not present on PATH");
  }
}

export async function commitWindowsQuickstartInstall(
  installation: WindowsQuickstartInstall,
): Promise<void> {
  await commitLegacyCoordinatorMigration(installation.migration);
  commitWindowsTlsInstall(installation.tls);
  installation.committed = true;
}

export async function rollbackWindowsQuickstartInstall(
  installation: WindowsQuickstartInstall,
  cause: unknown,
): Promise<void> {
  if (installation.committed) return;
  const rollbackErrors: unknown[] = [];
  const suspendedLifecycle = installation.migration !== null || installation.tls !== null;
  if (suspendedLifecycle) {
    try {
      await installation.manager.stop("coordinator");
    } catch (rollbackError) {
      rollbackErrors.push(rollbackError);
    }
  }
  try {
    await installation.manager.restore(installation.snapshot, {
      restoreLifecycleRoles: suspendedLifecycle ? [] : WINDOWS_SERVICE_ROLES,
      allowKeeperStop: true,
    });
  } catch (rollbackError) {
    rollbackErrors.push(rollbackError);
  }
  if (installation.tls) {
    try {
      await rollbackWindowsTlsInstall(installation.tls);
      installation.tls = null;
    } catch (rollbackError) {
      rollbackErrors.push(rollbackError);
    }
  }
  if (installation.migration) {
    try {
      await rollbackLegacyCoordinatorMigration(installation.migration);
      installation.migration = null;
    } catch (rollbackError) {
      rollbackErrors.push(rollbackError);
    }
  }
  if (suspendedLifecycle) {
    try {
      await installation.manager.restore(installation.snapshot, {
        restoreLifecycleRoles: WINDOWS_SERVICE_ROLES,
        allowKeeperStop: true,
      });
    } catch (rollbackError) {
      rollbackErrors.push(rollbackError);
    }
  }
  if (rollbackErrors.length > 0) {
    throw new AggregateError(
      [cause, ...rollbackErrors],
      "Windows quickstart failed and exact rollback was incomplete",
    );
  }
}
