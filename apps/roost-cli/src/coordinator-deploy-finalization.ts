// Proves coordinator and fleet convergence and records the global commit decision.
// Push marks fleet-converging only after target health, then calls begin finalization
// before any worker drops rollback state. Final cleanup requires an idempotent fleet
// finalizer so interrupted finalization cannot become a coordinator-only commit.
import { coordServiceLabel } from "@roost/shared/paths";
import { posixShellQuote } from "@roost/shared/shell-quote";
import { existsSync, lstatSync, readFileSync } from "node:fs";
import { dirname, isAbsolute, resolve } from "node:path";
import { durableRemove, flushDurablePath } from "@roost/shared/durability";
import { DeployFailure, run, type RunOptions } from "./deploy-exec.ts";
import { statusReport, type StatusReport } from "./status.ts";
import {
  checkpointCoordinatorFinalizationDecision,
  coordinatorInstallEnvironment,
  coordinatorRepoFromService,
  coordinatorRestartCommand,
  loadCoordinatorDeployJournal,
  writeCoordinatorDeployPhase,
  type CoordinatorDeployJournalContext,
  type CoordinatorDeployJournalV2,
} from "./coordinator-deploy-journal.ts";
import { retirePriorCoordinatorRelease } from "./coordinator-deploy-release.ts";

export const VERIFY_TIMEOUT_MS = 60_000;
export const VERIFY_POLL_MS = 1_000;

export type CoordinatorCommandRunner = (
  command: string[],
  options?: RunOptions,
) => Promise<{ exit: number; stdout: string; stderr: string }>;

export interface CoordinatorDeployRuntimeOptions {
  runCommand?: CoordinatorCommandRunner;
  readStatus?: () => Promise<StatusReport>;
  sleep?: (milliseconds: number) => Promise<void>;
  now?: () => number;
  verifyTimeoutMs?: number;
}

export interface CoordinatorDeployRuntime {
  runCommand: CoordinatorCommandRunner;
  readStatus: () => Promise<StatusReport>;
  sleep: (milliseconds: number) => Promise<void>;
  now: () => number;
  verifyTimeoutMs: number;
}

export function coordinatorDeployRuntime(
  options: CoordinatorDeployRuntimeOptions = {},
): CoordinatorDeployRuntime {
  return {
    runCommand: options.runCommand ?? run,
    readStatus: options.readStatus ?? statusReport,
    sleep: options.sleep ?? Bun.sleep,
    now: options.now ?? Date.now,
    verifyTimeoutMs: options.verifyTimeoutMs ?? VERIFY_TIMEOUT_MS,
  };
}

export function coordinatorReportIsOperational(report: StatusReport): boolean {
  return report.coord.reachable
    && report.coordAgentLoaded
    && report.tlsMode !== "missing";
}

export function coordinatorReportIsHealthy(report: StatusReport, expectedSha: string): boolean {
  return coordinatorReportIsOperational(report) && report.coord.gitSha === expectedSha;
}

export async function coordinatorStartupPolicyIsEnabled(
  platform: "darwin" | "linux",
  label: string = coordServiceLabel(process.env, platform),
  runCommand: CoordinatorCommandRunner = run,
): Promise<boolean> {
  if (platform === "linux") {
    const unit = label.endsWith(".service") ? label : `${label}.service`;
    const result = await runCommand(["bash", "-lc",
      `export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"; `
        + `systemctl --user is-enabled ${posixShellQuote(unit)}`,
    ], { quiet: true });
    return result.exit === 0 && result.stdout.trim() === "enabled";
  }
  const result = await runCommand(["bash", "-lc", "launchctl print-disabled gui/$(id -u)"], {
    quiet: true,
  });
  return result.exit === 0
    && !new RegExp(`"${label.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}"\\s*=>\\s*true`)
      .test(result.stdout);
}

function serviceTargets(
  journal: CoordinatorDeployJournalV2,
  platform: "darwin" | "linux",
  releasePath: string,
  sha: string,
): boolean {
  if (!existsSync(journal.servicePath) || !existsSync(releasePath)) return false;
  try {
    const definition = readFileSync(journal.servicePath, "utf8");
    const repo = coordinatorRepoFromService(definition, platform);
    if (!repo || !isAbsolute(repo) || resolve(repo) !== releasePath) return false;
    const environment = coordinatorInstallEnvironment(definition, platform);
    return (environment.ROOST_GIT_SHA ?? environment.GIT_SHA) === sha
      && environment.ROOST_COORDINATOR_DB === journal.databasePath;
  } catch {
    return false;
  }
}

export function currentServiceTargetsRelease(
  journal: CoordinatorDeployJournalV2,
  platform: "darwin" | "linux",
): boolean {
  return serviceTargets(journal, platform, journal.stagedReleasePath, journal.targetSha);
}

export function currentServiceTargetsPriorRelease(
  journal: CoordinatorDeployJournalV2,
  platform: "darwin" | "linux",
): boolean {
  if (!serviceTargets(journal, platform, journal.sourceReleasePath, journal.priorSha)) return false;
  try {
    const actual = readFileSync(journal.servicePath);
    const expected = Buffer.from(journal.priorDefinitionBase64, "base64");
    return actual.equals(expected)
      && (lstatSync(journal.servicePath).mode & 0o777) === journal.priorDefinitionMode;
  } catch {
    return false;
  }
}

export async function waitForCoordinatorProof(
  journal: CoordinatorDeployJournalV2,
  context: CoordinatorDeployJournalContext,
  expected: "prior" | "target",
  runtime: CoordinatorDeployRuntime,
): Promise<StatusReport | null> {
  const deadline = runtime.now() + runtime.verifyTimeoutMs;
  for (;;) {
    try {
      const report = await runtime.readStatus();
      const sha = expected === "prior" ? journal.priorSha : journal.targetSha;
      const definitionMatches = expected === "prior"
        ? currentServiceTargetsPriorRelease(journal, context.platform)
        : currentServiceTargetsRelease(journal, context.platform);
      if (definitionMatches
        && coordinatorReportIsHealthy(report, sha)
        && await coordinatorStartupPolicyIsEnabled(
          context.platform,
          coordServiceLabel(process.env, context.platform),
          runtime.runCommand,
        )) {
        return report;
      }
    } catch {
      // A status/startup probe can race the service transition; the deadline is authoritative.
    }
    if (runtime.now() >= deadline) return null;
    await runtime.sleep(VERIFY_POLL_MS);
  }
}

export async function markCoordinatorFleetConverging(
  journalPath: string,
  context: CoordinatorDeployJournalContext,
  options: CoordinatorDeployRuntimeOptions = {},
): Promise<CoordinatorDeployJournalV2> {
  const journal = loadCoordinatorDeployJournal(journalPath, context);
  if (!journal || journal.phase !== "activating") {
    throw new DeployFailure(5, "coordinator target health requires an activating journal");
  }
  const runtime = coordinatorDeployRuntime(options);
  if (!await waitForCoordinatorProof(journal, context, "target", runtime)) {
    throw new DeployFailure(8, `coordinator did not prove healthy at ${journal.targetSha}`);
  }
  return writeCoordinatorDeployPhase(journalPath, journal, "fleet-converging");
}

export function coordinatorFleetConvergenceProblems(
  journal: CoordinatorDeployJournalV2,
  context: CoordinatorDeployJournalContext,
  report: StatusReport,
): string[] {
  const problems: string[] = [];
  if (!currentServiceTargetsRelease(journal, context.platform)
    || !coordinatorReportIsHealthy(report, journal.targetSha)) {
    problems.push(`coordinator is not healthy at ${journal.targetSha}`);
  }
  const actualFingerprints = report.workers.map((worker) => worker.fingerprint).sort();
  if (actualFingerprints.length !== journal.targetWorkerFingerprints.length
    || actualFingerprints.some(
      (fingerprint, index) => fingerprint !== journal.targetWorkerFingerprints[index],
    )) {
    problems.push("registered worker set does not exactly match the rollout target set");
  }
  const seen = new Set<string>();
  for (const worker of report.workers) {
    if (seen.has(worker.fingerprint)) {
      problems.push(`worker ${worker.fingerprint} appears more than once`);
      continue;
    }
    seen.add(worker.fingerprint);
    if (worker.stale) problems.push(`worker ${worker.fingerprint} is stale`);
    if (worker.gitSha !== journal.targetSha) {
      problems.push(`worker ${worker.fingerprint} does not report ${journal.targetSha}`);
    }
    if (worker.keeperState !== "current") {
      problems.push(`worker ${worker.fingerprint} keeper is ${worker.keeperState}`);
    }
  }
  return problems;
}

export async function beginCoordinatorDeployFinalization(
  journalPath: string,
  context: CoordinatorDeployJournalContext,
  options: CoordinatorDeployRuntimeOptions = {},
): Promise<CoordinatorDeployJournalV2> {
  const journal = loadCoordinatorDeployJournal(journalPath, context);
  if (!journal || journal.phase !== "fleet-converging") {
    throw new DeployFailure(5, "coordinator finalization requires fleet-converging state");
  }
  const runtime = coordinatorDeployRuntime(options);
  const report = await runtime.readStatus();
  const problems = coordinatorFleetConvergenceProblems(journal, context, report);
  if (!await coordinatorStartupPolicyIsEnabled(
    context.platform,
    coordServiceLabel(process.env, context.platform),
    runtime.runCommand,
  )) {
    problems.push("coordinator automatic startup is disabled");
  }
  if (problems.length > 0) {
    throw new DeployFailure(8, `fleet cannot finalize:\n${problems.join("\n")}`);
  }
  return checkpointCoordinatorFinalizationDecision(journalPath, journal);
}

async function ensureFinalizingTarget(
  journal: CoordinatorDeployJournalV2,
  context: CoordinatorDeployJournalContext,
  runtime: CoordinatorDeployRuntime,
): Promise<void> {
  if (await waitForCoordinatorProof(journal, context, "target", {
    ...runtime,
    verifyTimeoutMs: 0,
  })) return;
  if (!currentServiceTargetsRelease(journal, context.platform)) {
    throw new DeployFailure(8, "finalizing coordinator service no longer targets the rollout release");
  }
  const restarted = await runtime.runCommand(
    ["bash", "-lc", coordinatorRestartCommand(
      journal.servicePath,
      context.platform,
      coordServiceLabel(process.env, context.platform),
    )],
    { cwd: context.releaseRoot, quiet: true },
  );
  if (restarted.exit !== 0
    || !await waitForCoordinatorProof(journal, context, "target", runtime)) {
    throw new DeployFailure(8, `finalizing coordinator did not prove ${journal.targetSha}`);
  }
}

export async function finalizeCoordinatorDeploy(
  journalPath: string,
  context: CoordinatorDeployJournalContext,
  finishFleetFinalization: (journal: CoordinatorDeployJournalV2) => Promise<void>,
  options: CoordinatorDeployRuntimeOptions = {},
): Promise<void> {
  const journal = loadCoordinatorDeployJournal(journalPath, context);
  if (!journal || journal.phase !== "finalizing") {
    throw new DeployFailure(5, "coordinator cleanup requires a finalizing journal");
  }
  const runtime = coordinatorDeployRuntime(options);
  await ensureFinalizingTarget(journal, context, runtime);
  await finishFleetFinalization(journal);
  await flushDurablePath(journal.servicePath);
  await flushDurablePath(dirname(journal.servicePath));
  await retirePriorCoordinatorRelease(context.releaseRoot, journal, context.platform);
  await durableRemove(journal.databaseSnapshotPath);
  await durableRemove(journalPath);
}
