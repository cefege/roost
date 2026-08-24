// Inverse replay for the Windows update broker: the deterministic rollback
// state machine, prior SCM/lifecycle restore helpers, service-definitions
// document writer shared with the commit path, and the phase lattices that
// order every forward and rollback step.
//
// Callers: windows-update-broker.ts (crash recovery, failure paths, cleanup,
// and the forward checkpoint writer). Depends on windows-update-assets.ts,
// windows-update-stable-artifacts.ts, windows-update-journal.ts, and
// windows-path-safety.ts. Broker types are imported type-only, so no runtime
// cycle exists.

import { createHash } from "node:crypto";
import { basename, dirname, join } from "node:path";
import {
  durableReplace,
  flushDurablePath,
} from "@roost/shared/durability";
import {
  appendWindowsUpdateProgress,
  replaceWindowsUpdaterArtifact,
  sha256Hex,
  type WindowsUpdateForwardPhase,
  type WindowsUpdateJournalV2,
  type WindowsUpdateRollbackPhase,
} from "./windows-update-journal.ts";
import { depsNow, errorText } from "./windows-path-safety.ts";
import { directoryExists, nativeTransactionOps } from "./windows-update-assets.ts";
import { normalizeSddl, restoreStableArtifacts } from "./windows-update-stable-artifacts.ts";
import type {
  WindowsServiceDefinition,
  RoostServiceRole,
} from "./windows-service-types.ts";
import { WINDOWS_SERVICE_ROLES } from "./windows-service-types.ts";
import type {
  WindowsUpdateBrokerDeps,
  WindowsUpdateServiceManager,
} from "./windows-update-broker.ts";


export async function assertCurrentManifestSecurity(
  journal: WindowsUpdateJournalV2,
  deps: WindowsUpdateBrokerDeps,
  raw: string,
): Promise<void> {
  const bytes = Buffer.from(raw);
  const expected = { sha256: sha256Hex(bytes), size: bytes.byteLength };
  const inspected = await nativeTransactionOps(deps.native).inspectArtifact(
    journal.paths.currentManifestPath,
    "current",
    expected,
  );
  if (
    normalizeSddl(inspected.sddl)
      !== normalizeSddl(journal.currentManifestSnapshot.securityDescriptor)
  ) {
    throw new Error("current manifest owner/DACL did not restore exactly");
  }
}

async function restoreCurrentManifest(
  journal: WindowsUpdateJournalV2,
  deps: WindowsUpdateBrokerDeps,
): Promise<void> {
  const raw = journal.currentManifest.priorRaw;
  if (raw === null) {
    throw new Error("stable Windows rollback requires prior current manifest bytes");
  }
  await writeCurrentManifest(journal.paths.currentManifestPath, raw, deps);
  await assertCurrentManifestSecurity(journal, deps, raw);
}

export async function writeCurrentManifest(
  path: string,
  contents: string | null,
  deps: WindowsUpdateBrokerDeps,
): Promise<void> {
  if (deps.writeCurrentManifest) return await deps.writeCurrentManifest(path, contents);
  if (contents === null) {
    throw new Error("installed stable Windows topology requires a prior current manifest");
  }
  if (deps.native.replaceArtifact) {
    await deps.native.replaceArtifact(path, "current", contents);
    return;
  }
  if (process.platform === "win32") {
    throw new Error("constrained Windows current-manifest replacement is unavailable");
  }
  await replaceWindowsUpdaterArtifact(path, contents, "current");
}

export const ACTIVE_ROLES = ["coordinator", "worker"] as const satisfies readonly RoostServiceRole[];
export const STOP_ORDER = ["worker", "coordinator", "keeper"] as const satisfies readonly RoostServiceRole[];
export const START_ORDER = ["keeper", "coordinator", "worker"] as const satisfies readonly RoostServiceRole[];

export const FORWARD_ORDER: readonly import("./windows-update-journal.ts").WindowsUpdateForwardPhase[] = [
  "prepared",
  "broker-started",
  "assets-staged",
  "stable-artifacts-snapshotted",
  "services-stopped",
  "stable-artifacts-promoted",
  "updater-config-switched",
  "current-manifest-switched",
  "services-restored",
  "health-proven",
  "committed",
  "cleanup-complete",
];

const ROLLBACK_ORDER = [
  "rollback-started",
  "rollback-services-stopped",
  "rollback-stable-artifacts-restored",
  "rollback-configs-restored",
  "rollback-current-manifest-restored",
  "rollback-services-restored",
  "rolled-back",
] as const satisfies readonly WindowsUpdateRollbackPhase[];

export function isForward(phase: WindowsUpdateJournalV2["phase"]): phase is import("./windows-update-journal.ts").WindowsUpdateForwardPhase {
  return FORWARD_ORDER.includes(phase as import("./windows-update-journal.ts").WindowsUpdateForwardPhase);
}

function isRollback(
  phase: WindowsUpdateJournalV2["phase"],
): phase is WindowsUpdateRollbackPhase {
  return ROLLBACK_ORDER.some((candidate) => candidate === phase);
}

export function phaseAtLeast(
  phase: WindowsUpdateJournalV2["phase"],
  expected: import("./windows-update-journal.ts").WindowsUpdateForwardPhase,
): boolean {
  return isForward(phase) && FORWARD_ORDER.indexOf(phase) >= FORWARD_ORDER.indexOf(expected);
}

function rollbackBefore(
  journal: WindowsUpdateJournalV2,
  phase: WindowsUpdateRollbackPhase,
): boolean {
  const currentIndex = ROLLBACK_ORDER.findIndex((candidate) => candidate === journal.phase);
  return currentIndex < ROLLBACK_ORDER.indexOf(phase);
}

export async function checkpoint(
  journal: WindowsUpdateJournalV2,
  deps: WindowsUpdateBrokerDeps,
  phase: Parameters<typeof appendWindowsUpdateProgress>[1],
  message: string,
  state: "forward" | "rolling-back" = "forward",
): Promise<WindowsUpdateJournalV2> {
  const next = appendWindowsUpdateProgress(journal, phase, message, { state, now: depsNow(deps) });
  await deps.store.save(next);
  return next;
}

export function uniqueRoles(roles: readonly RoostServiceRole[]): RoostServiceRole[] {
  return [...new Set(roles)];
}

export function serviceDefinitionsDocument(
  definitions: readonly WindowsServiceDefinition[],
): string {
  const byRole = Object.fromEntries(
    definitions.map((definition) => [definition.role, definition]),
  ) as Partial<Record<RoostServiceRole, WindowsServiceDefinition>>;
  const services = Object.fromEntries(WINDOWS_SERVICE_ROLES.map((role) => {
    const definition = byRole[role];
    if (!definition) throw new Error(`Windows service definitions omit ${role}`);
    return [role, definition];
  })) as Record<RoostServiceRole, WindowsServiceDefinition>;
  return `${JSON.stringify({ schemaVersion: 2, services }, null, 2)}\n`;
}

export async function writeServiceDefinitions(
  currentManifestPath: string,
  definitions: readonly WindowsServiceDefinition[],
): Promise<void> {
  await replaceWindowsUpdaterArtifact(
    join(dirname(currentManifestPath), "service-definitions.json"),
    serviceDefinitionsDocument(definitions),
    "control",
  );
}

export async function restorePriorServiceConfiguration(
  journal: WindowsUpdateJournalV2,
  services: WindowsUpdateServiceManager,
): Promise<void> {
  if (!services.restore) {
    if (process.platform === "win32") throw new Error("exact Windows SCM restore operation is unavailable");
    return;
  }
  await services.restore(journal.serviceSnapshot, {
    restoreLifecycleRoles: [],
    allowKeeperStop: true,
  });
}

export async function restorePriorLifecycle(
  journal: WindowsUpdateJournalV2,
  services: WindowsUpdateServiceManager,
): Promise<RoostServiceRole[]> {
  if (services.restore) {
    await services.restore(journal.serviceSnapshot, {
      restoreLifecycleRoles: START_ORDER,
      allowKeeperStop: true,
    });
    return START_ORDER.filter((role) => journal.runningBefore[role]);
  }
  const restored: RoostServiceRole[] = [];
  for (const role of START_ORDER) {
    const actual = await services.query(role);
    if (journal.runningBefore[role] && actual.state !== "running") {
      await services.start(role);
      restored.push(role);
    } else if (!journal.runningBefore[role] && actual.state === "running") {
      await services.stop(role, { timeoutMs: 15_000 });
    }
  }
  return restored;
}

export async function removeAdmissionMarker(_journal: WindowsUpdateJournalV2): Promise<void> {
  // Pending request capability is consumed immediately after the durable journal save.
}

async function rollbackVersionAssets(journal: WindowsUpdateJournalV2): Promise<void> {
  const versionDir = journal.paths.newVersionDir;
  if (versionDir === journal.paths.priorVersionDir || !await directoryExists(versionDir)) return;
  const folded = versionDir.replaceAll("/", "\\").toLowerCase();
  const referenced = Object.values(journal.serviceSnapshot).some((snapshot) =>
    snapshot.imagePath?.replaceAll("/", "\\").toLowerCase().includes(folded)
  ) || process.execPath.replaceAll("/", "\\").toLowerCase().startsWith(`${folded}\\`);
  if (referenced) return;
  const rollbackId = createHash("sha256").update(journal.transactionId).digest("hex").slice(0, 16);
  const quarantine = join(dirname(versionDir), `.rolled-back-${basename(versionDir)}-${rollbackId}`);
  if (await directoryExists(quarantine)) {
    throw new Error(`rollback quarantine already exists while version remains installed: ${quarantine}`);
  }
  await durableReplace(versionDir, quarantine);
  await flushDurablePath(dirname(versionDir));
}

export async function rollback(
  initial: WindowsUpdateJournalV2,
  deps: WindowsUpdateBrokerDeps,
): Promise<WindowsUpdateJournalV2> {
  if (initial.stableArtifacts.mode === "proof-only") {
    const terminal = appendWindowsUpdateProgress(
      initial,
      "rolled-back",
      "same-digest proof failed without mutating the machine",
      {
        state: "rolled-back",
        terminal: true,
        success: false,
        error: initial.failure?.error,
        now: depsNow(deps),
      },
    );
    await deps.store.save(terminal);
    await removeAdmissionMarker(terminal);
    return terminal;
  }
  let journal = initial;
  try {
    const touched = phaseAtLeast(
      journal.failure?.forwardPhase ?? "prepared",
      "stable-artifacts-snapshotted",
    );
    if (rollbackBefore(journal, "rollback-services-stopped")) {
      if (touched) {
        for (const role of STOP_ORDER) {
          if ((await deps.services.query(role)).state === "running") {
            await deps.services.stop(role, { timeoutMs: 15_000 });
          }
        }
      }
      journal = await checkpoint(
        journal,
        deps,
        "rollback-services-stopped",
        touched ? "stopped mutable roles before inverse replay" : "forward mutation never began",
        "rolling-back",
      );
    }
    if (rollbackBefore(journal, "rollback-stable-artifacts-restored")) {
      if (touched) await restoreStableArtifacts(journal, deps);
      journal = await checkpoint(
        journal,
        deps,
        "rollback-stable-artifacts-restored",
        touched ? "restored exact prior stable bytes and full DACLs" : "stable artifacts were untouched",
        "rolling-back",
      );
    }
    if (rollbackBefore(journal, "rollback-configs-restored")) {
      await restorePriorServiceConfiguration(journal, deps.services);
      await writeServiceDefinitions(
        journal.paths.currentManifestPath,
        Object.values(journal.priorServiceDefinitions),
      );
      await rollbackVersionAssets(journal);
      journal = await checkpoint(
        journal,
        deps,
        "rollback-configs-restored",
        "restored exact prior SCM configuration and security",
        "rolling-back",
      );
    }
    if (rollbackBefore(journal, "rollback-current-manifest-restored")) {
      await restoreCurrentManifest(journal, deps);
      journal = await checkpoint(
        journal,
        deps,
        "rollback-current-manifest-restored",
        "restored exact prior current manifest bytes",
        "rolling-back",
      );
    }
    if (rollbackBefore(journal, "rollback-services-restored")) {
      const roles = await restorePriorLifecycle(journal, deps.services);
      for (const role of ACTIVE_ROLES) {
        if (journal.runningBefore[role]) await deps.health.prove(role, journal, "rollback");
      }
      journal = { ...journal, restoredRoles: uniqueRoles([...journal.restoredRoles, ...roles]) };
      journal = await checkpoint(
        journal,
        deps,
        "rollback-services-restored",
        "restored and proved the exact prior lifecycle and build vector",
        "rolling-back",
      );
    }
    journal = appendWindowsUpdateProgress(
      journal,
      "rolled-back",
      "Windows update rolled back deterministically",
      {
        state: "rolled-back",
        terminal: true,
        success: false,
        error: journal.failure?.error,
        now: depsNow(deps),
      },
    );
    await deps.store.save(journal);
    await removeAdmissionMarker(journal);
    return journal;
  } catch (error) {
    const phase = isRollback(journal.phase) ? journal.phase : "rollback-started";
    journal = {
      ...journal,
      rollbackFailure: { phase, error: errorText(error), at: depsNow(deps).toISOString() },
    };
    journal = appendWindowsUpdateProgress(
      journal,
      phase,
      "rollback interrupted; SCM restart will resume this journal",
      { state: "rolling-back", error: errorText(error), now: depsNow(deps) },
    );
    await deps.store.save(journal);
    throw error;
  }
}
