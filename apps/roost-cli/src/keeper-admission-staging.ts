// Collapses one keeper-update admission outcome into what a deploy driver is
// allowed to stage. Called by deploy-macos.ts, deploy-linux.ts and
// deploy-local.ts; direct-keeper-update.ts produces the outcomes it consumes.
// The three unproven outcomes stay distinct because only the bootstrap one may
// stage over a worker service that is already installed.

import type { JournaledKeeperUpdateV1 } from "@roost/shared/keeper-update";

export interface DirectKeeperAdmission {
  workerFingerprint: string;
  keeperUpdate: JournaledKeeperUpdateV1;
}

/** Deploy-time admission result. `admitted` carries the journaled update; the
 * other three are the cases where the registry holds no usable proof, and they
 * are separate values so a deploy driver can tell "no proof can exist" apart
 * from "proof was refused" — a refusal throws and never reaches here. */
export type DirectKeeperAdmissionOutcome =
  | ({ outcome: "admitted" } & DirectKeeperAdmission)
  | { outcome: "unregistered" }
  | { outcome: "proof-stale"; workerLabel: string }
  | { outcome: "runtime-unreported"; workerLabel: string };

export type KeeperAdmissionPlatform = "macOS" | "Linux" | "local";

export interface KeeperAdmissionStaging {
  keeperUpdate: JournaledKeeperUpdateV1 | null;
  workerFingerprint: string | null;
  /** Refusal to report when the target still has a worker service installed.
   * `null` is the bootstrap allowance: the registered worker can never report a
   * keeper runtime, so demanding proof would make it permanently unupgradable. */
  installedServiceRefusal: string | null;
}

export function unprovenInstalledServiceRefusal(
  platform: KeeperAdmissionPlatform,
): string {
  return `existing ${platform} worker requires keeper update admission before staging`;
}

export function keeperAdmissionStaging(
  host: string,
  platform: KeeperAdmissionPlatform,
  resolved: DirectKeeperAdmissionOutcome,
): KeeperAdmissionStaging {
  switch (resolved.outcome) {
    case "admitted":
      return {
        keeperUpdate: resolved.keeperUpdate,
        workerFingerprint: resolved.workerFingerprint,
        installedServiceRefusal: null,
      };
    case "runtime-unreported":
      // The staged release carries no journaled keeper update, so this deploy
      // performs no keeper mutation at all; the worker's own boot admission
      // (boot-keeper.ts) still refuses to replace a keeper holding live
      // channels. Demanding proof here instead just pins the worker on the
      // build that cannot produce it.
      console.log(
        `>> keeper admission bootstrap on ${host}: worker ${resolved.workerLabel} reports no keeper`
        + " runtime, so no proof can exist for it; staging without a journaled keeper update."
        + " The worker's own boot keeper admission is the remaining fence.",
      );
      return {
        keeperUpdate: null,
        workerFingerprint: null,
        installedServiceRefusal: null,
      };
    case "proof-stale":
      // An installed service can start and prove admission at any moment, so a
      // stale row only waives staging on a host where nothing will start.
      return {
        keeperUpdate: null,
        workerFingerprint: null,
        installedServiceRefusal:
          `existing ${platform} worker ${resolved.workerLabel} has a stale keeper update proof;`
          + ` start the worker on ${host} so it can prove admission`,
      };
    case "unregistered":
      return {
        keeperUpdate: null,
        workerFingerprint: null,
        installedServiceRefusal: unprovenInstalledServiceRefusal(platform),
      };
  }
}
