// Collapses one keeper-update admission outcome, plus the evidence the target
// reports about itself, into what a deploy driver is allowed to stage. Called
// by deploy-macos.ts, deploy-linux.ts and deploy-local.ts;
// direct-keeper-update.ts produces the outcomes it consumes. An unproven
// outcome refuses only while the target still runs something a staged release
// could destroy, so a worker that is down stays repairable by deploy.

import type { JournaledKeeperUpdateV1 } from "@roost/shared/keeper-update";
import { posixShellQuote } from "@roost/shared/shell-quote";
import { MUX_KEEPER_ENDPOINT_NAME } from "../../worker/src/keeper/keeper-pool-config.ts";
import { workerServiceIsRunning, type DeployWorkerOs } from "./deploy-exec.ts";
import { verifyWorkerCmd } from "./service-ctl.ts";

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
   * keeper runtime, so demanding proof would make it permanently unupgradable.
   * A non-null refusal is a claim about the registry, not about the host — the
   * target evidence below decides whether it actually stands. */
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
      // A row the coordinator stopped refreshing describes the coordinator's
      // knowledge, not the host: the worker may be running and unreachable, or
      // dead. Only the target can tell those apart, so the refusal is stated
      // without a remedy and the evidence probe below decides whether it holds.
      return {
        keeperUpdate: null,
        workerFingerprint: null,
        installedServiceRefusal:
          `existing ${platform} worker ${resolved.workerLabel} has a stale keeper update proof`,
      };
    case "unregistered":
      return {
        keeperUpdate: null,
        workerFingerprint: null,
        installedServiceRefusal: unprovenInstalledServiceRefusal(platform),
      };
  }
}

/** What the target reported about itself. Absence is only ever read from a
 * positive observation: `serviceObserved` / `processesObserved` false means the
 * probe could not tell, which is never a licence to stage. A keeper socket file
 * is deliberately not part of this — it outlives the keeper that created it, so
 * it can neither prove nor disprove anything the process counts do not. */
interface TargetWorkerEvidence {
  serviceObserved: boolean;
  serviceInstalled: boolean;
  /** The service manager answered at all. False means launchd/systemd was
   * unreachable, which reads identically to "not running" in its output. */
  serviceStateObserved: boolean;
  serviceRunning: boolean;
  processesObserved: boolean;
  keeperProcesses: number;
  /** Processes those keepers are parenting — one per live PTY channel. */
  keeperChannelProcesses: number;
}

export interface TargetWorkerEvidenceProbe {
  host: string;
  os: DeployWorkerOs;
  /** Installed service definition: absolute, or relative to the target `$HOME`. */
  serviceSpec: string;
  execute: (command: string) => Promise<{ exit: number; stdout: string; stderr: string }>;
}

type InstalledServiceVerdict =
  | { permitted: true; evidence: string }
  | { permitted: false; refusal: string };

// The keeper's endpoint address is the one argument carried by both its
// from-source and its packaged spawn form. `pgrep -f` also sees this deploy's
// own ssh command line, so the leading character is bracketed: the pattern then
// cannot match the literal text that carries it.
const KEEPER_PROCESS_PATTERN =
  `[${MUX_KEEPER_ENDPOINT_NAME.slice(0, 1)}]${MUX_KEEPER_ENDPOINT_NAME.slice(1)}\\.sock`;

/** launchd reports an unloaded job with the same failing exit as an unreachable
 * launchd, so darwin corroborates with a domain query that succeeds either way;
 * `systemctl show` already exits 0 for a unit it has never heard of. */
function serviceStateObservationCommand(os: DeployWorkerOs): string {
  return os === "darwin"
    ? `if test "$service_status" -eq 0 || launchctl print-disabled gui/$(id -u) >/dev/null 2>&1; `
      + `then echo RoostServiceState=observed; fi; `
    : `if test "$service_status" -eq 0; then echo RoostServiceState=observed; fi; `;
}

function targetWorkerEvidenceCommand(
  os: DeployWorkerOs,
  serviceSpec: string,
): string {
  return `spec=${posixShellQuote(serviceSpec)}; `
    + `case "$spec" in /*) service="$spec";; *) service="$HOME/$spec";; esac; `
    + `if test -e "$service" || test -L "$service"; `
    + `then echo RoostServiceInstalled=yes; else echo RoostServiceInstalled=no; fi; `
    + `service_output=$( ( ${verifyWorkerCmd(os)} ) 2>&1 ); service_status=$?; `
    + `printf '%s\\n' "$service_output"; `
    + serviceStateObservationCommand(os)
    // The checks above need no extra tooling, so they are emitted before the
    // process probe bails out on a host without pgrep.
    + `command -v pgrep >/dev/null 2>&1 || exit 0; `
    + `keeper_pids=$(pgrep -f ${posixShellQuote(KEEPER_PROCESS_PATTERN)} || true); `
    + `echo "RoostKeeperProcesses=$(printf '%s' "$keeper_pids" | grep -c . || true)"; `
    + `keeper_list=$(printf '%s' "$keeper_pids" | tr '\\n' ',' | sed 's/,*$//'); `
    + `if test -z "$keeper_list"; then keeper_children=0; `
    + `else keeper_children=$(pgrep -P "$keeper_list" | grep -c . || true); fi; `
    + `echo "RoostKeeperChannelProcesses=$keeper_children"; `
    + `echo RoostTargetEvidence=complete`;
}

function parseTargetWorkerEvidence(
  result: Readonly<{ exit: number; stdout: string }>,
  os: DeployWorkerOs,
): TargetWorkerEvidence {
  const installed = /^RoostServiceInstalled=(yes|no)$/m.exec(result.stdout);
  const keepers = /^RoostKeeperProcesses=(\d+)$/m.exec(result.stdout);
  const children = /^RoostKeeperChannelProcesses=(\d+)$/m.exec(result.stdout);
  const transportOk = result.exit === 0;
  return {
    serviceObserved: transportOk && installed !== null,
    serviceInstalled: installed?.[1] === "yes",
    serviceStateObserved: transportOk
      && /^RoostServiceState=observed$/m.test(result.stdout),
    serviceRunning: workerServiceIsRunning(result.stdout, os),
    processesObserved: transportOk
      && keepers !== null
      && children !== null
      && /^RoostTargetEvidence=complete$/m.test(result.stdout),
    keeperProcesses: Number(keepers?.[1] ?? 0),
    keeperChannelProcesses: Number(children?.[1] ?? 0),
  };
}

function installedServiceVerdict(
  refusal: string,
  host: string,
  evidence: Readonly<TargetWorkerEvidence>,
): InstalledServiceVerdict {
  if (!evidence.serviceObserved) {
    return {
      permitted: false,
      refusal: `${refusal}; ${host} did not report whether a worker service is installed`,
    };
  }
  if (!evidence.serviceInstalled) {
    return { permitted: true, evidence: `no worker service is installed on ${host}` };
  }
  if (!evidence.serviceStateObserved) {
    return {
      permitted: false,
      refusal: `${refusal}; the service manager on ${host} did not report the worker's state`,
    };
  }
  if (evidence.serviceRunning) {
    return {
      permitted: false,
      refusal: `${refusal}; the worker service on ${host} is running and can prove admission`,
    };
  }
  if (!evidence.processesObserved) {
    return {
      permitted: false,
      refusal: `${refusal}; ${host} could not prove that no keeper is holding channels`,
    };
  }
  if (evidence.keeperChannelProcesses > 0) {
    return {
      permitted: false,
      refusal: `${refusal}; a keeper on ${host} still holds `
        + `${evidence.keeperChannelProcesses} channel process(es)`,
    };
  }
  return {
    permitted: true,
    evidence: `the worker service on ${host} is installed but not running, and its `
      + `${evidence.keeperProcesses} keeper process(es) hold no channel`,
  };
}

/** The refusal a deploy driver must fail on, or `null` once the target has
 * proven it runs nothing the staged release could destroy. */
export async function installedServiceRefusalAfterTargetEvidence(
  refusal: string,
  probe: Readonly<TargetWorkerEvidenceProbe>,
): Promise<string | null> {
  const result = await probe.execute(
    targetWorkerEvidenceCommand(probe.os, probe.serviceSpec),
  );
  const verdict = installedServiceVerdict(
    refusal,
    probe.host,
    parseTargetWorkerEvidence(result, probe.os),
  );
  if (!verdict.permitted) return verdict.refusal;
  console.log(
    `>> keeper admission on ${probe.host}: staging permitted because ${verdict.evidence}.`,
  );
  return null;
}
