// Stable-artifact (Shawl + launch-current launcher) transaction operations:
// exact prior-byte capture with SDDL proofs, constrained promotion, and
// byte-exact restore, all through held-handle native ops.
//
// Callers: windows-update-broker.ts (forward snapshots/promotion and the
// post-promotion proof), windows-update-rollback.ts (inverse replay).
// Depends on windows-update-assets.ts (nativeTransactionOps/verifyFile) and
// windows-update-journal.ts types.

import { join } from "node:path";
import type {
  WindowsStableArtifactPlan,
  WindowsUpdateJournalV2,
  WindowsUpdaterPersistenceProfile,
} from "./windows-update-journal.ts";
import { nativeTransactionOps, verifyFile } from "./windows-update-assets.ts";
import type { WindowsUpdateBrokerDeps } from "./windows-update-broker.ts";

export type StableArtifactName = "shawl" | "launcher";
type StableArtifactEntry = readonly [
  StableArtifactName,
  WindowsStableArtifactPlan,
  Extract<WindowsUpdaterPersistenceProfile, "stable-shawl" | "stable-launcher">,
];

export function stableArtifactEntries(journal: WindowsUpdateJournalV2): readonly StableArtifactEntry[] {
  return [
    ["shawl", journal.stableArtifacts.shawl, "stable-shawl"],
    ["launcher", journal.stableArtifacts.launcher, "stable-launcher"],
  ];
}

export function normalizeSddl(value: string): string {
  return value.replace(/\s+/g, "").toUpperCase();
}

export async function snapshotStableArtifacts(
  journal: WindowsUpdateJournalV2,
  deps: WindowsUpdateBrokerDeps,
): Promise<WindowsUpdateJournalV2> {
  if (journal.stableArtifacts.mode === "proof-only") {
    await verifyStableArtifacts(journal, deps);
    return journal;
  }
  const native = nativeTransactionOps(deps.native);
  const captured: Partial<Record<StableArtifactName, WindowsStableArtifactPlan>> = {};
  for (const [name, plan, profile] of stableArtifactEntries(journal)) {
    const source = await native.inspectArtifact(plan.stablePath, profile);
    if (!source.sddl.trim()) throw new Error(`${name} held stable security proof is empty`);
    const expected = { sha256: source.sha256, size: source.size };
    const backupPath = join(journal.stableArtifacts.backupDir, `${name}.bak`);
    let backupReady = false;
    try {
      await native.inspectArtifact(backupPath, "private", expected);
      backupReady = true;
    } catch {
      // Missing is the ordinary first-pass case. Copy is constrained and the
      // post-failure inspect closes the crash/race gap if another pass won.
    }
    if (!backupReady) {
      try {
        const copied = await native.copyArtifact(
          plan.stablePath,
          backupPath,
          profile,
          "private",
          expected,
        );
        if (
          copied.sha256 !== source.sha256
          || copied.size !== source.size
          || normalizeSddl(copied.sddl) !== normalizeSddl(source.sddl)
        ) {
          throw new Error(`${name} stable backup source proof changed during copy`);
        }
      } catch (copyError) {
        try {
          await native.inspectArtifact(backupPath, "private", expected);
        } catch {
          throw copyError;
        }
      }
    }
    const sourceAfter = await native.inspectArtifact(plan.stablePath, profile, expected);
    if (normalizeSddl(sourceAfter.sddl) !== normalizeSddl(source.sddl)) {
      throw new Error(`${name} stable owner/DACL changed while snapshotting`);
    }
    captured[name] = {
      ...plan,
      prior: {
        existed: true,
        backupPath,
        sha256: source.sha256,
        size: source.size,
        securityDescriptor: source.sddl,
      },
    };
  }
  return {
    ...journal,
    stableArtifacts: {
      ...journal.stableArtifacts,
      shawl: captured.shawl!,
      launcher: captured.launcher!,
    },
  };
}

export async function promoteStableArtifacts(
  journal: WindowsUpdateJournalV2,
  deps: WindowsUpdateBrokerDeps,
): Promise<void> {
  if (journal.stableArtifacts.mode === "proof-only") {
    await verifyStableArtifacts(journal, deps);
    return;
  }
  const native = nativeTransactionOps(deps.native);
  for (const [, plan, profile] of stableArtifactEntries(journal)) {
    await native.copyArtifact(
      plan.releasePath,
      plan.stablePath,
      "release",
      profile,
      { sha256: plan.sha256, size: plan.size },
    );
    await verifyFile(plan.stablePath, plan.sha256, plan.size);
    await deps.native.verifyAuthenticode(plan.stablePath, journal.signedManifest.publisherSha256);
  }
}

export async function restoreStableArtifacts(
  journal: WindowsUpdateJournalV2,
  deps: WindowsUpdateBrokerDeps,
): Promise<void> {
  if (journal.stableArtifacts.mode === "proof-only") return;
  const native = nativeTransactionOps(deps.native);
  for (const [name, plan, profile] of stableArtifactEntries(journal)) {
    if (!plan.prior || !plan.prior.existed) {
      throw new Error(`cannot restore ${name}: durable prior stable snapshot is missing`);
    }
    await native.copyArtifact(
      plan.prior.backupPath,
      plan.stablePath,
      "private",
      profile,
      { sha256: plan.prior.sha256, size: plan.prior.size },
    );
    const restored = await native.inspectArtifact(
      plan.stablePath,
      profile,
      { sha256: plan.prior.sha256, size: plan.prior.size },
    );
    if (normalizeSddl(restored.sddl) !== normalizeSddl(plan.prior.securityDescriptor)) {
      throw new Error(`${name} stable DACL did not restore exactly`);
    }
  }
}

export async function verifyStableArtifacts(
  journal: WindowsUpdateJournalV2,
  deps: WindowsUpdateBrokerDeps,
): Promise<void> {
  const native = nativeTransactionOps(deps.native);
  for (const [, plan, profile] of stableArtifactEntries(journal)) {
    await native.inspectArtifact(
      plan.stablePath,
      profile,
      { sha256: plan.sha256, size: plan.size },
    );
    await deps.native.verifyAuthenticode(plan.stablePath, journal.signedManifest.publisherSha256);
  }
}
