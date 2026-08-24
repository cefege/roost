// `roost push` — publish one clean commit, deploy every registered worker,
// update the coordinator's actual source checkout, and prove every process
// reports that commit before returning success.

import { cpSync, existsSync, lstatSync, mkdirSync, readdirSync, readFileSync, realpathSync, rmSync } from "node:fs";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { coordServiceLabel, coordServicePath, roostServiceDir, workerServicePath } from "@roost/shared/paths";
import { durableRemove, durableWriteFile, flushDurablePath } from "@roost/shared/durability";
import { acquireMachineTransaction } from "./machine-transaction.ts";
import {
  parsePosixServiceEnvironment,
  parseSystemdServiceDirective,
} from "./deploy-plist-env.ts";
import { deploy } from "./deploy.ts";
import {
  DeployFailure,
  POSIX_WORKER_DEPLOY_JOURNAL_PATHS,
  failDeploy,
  resolveGitPublishTargetOrDie,
  resolveLocalGitShaOrDie,
  resolvePublishedGitShaOrDie,
  run,
  runOrDie,
} from "./deploy-exec.ts";
import {
  statusReport,
  workerInventory,
  type StatusReport,
  type WorkerStatus,
} from "./status.ts";
import { parseWindowsReleaseManifest } from "./windows/windows-update-journal.ts";
import { fetchAndVerifyReleaseAsset, WINDOWS_RELEASE_MANIFEST_ASSET } from "./update.ts";
import {
  coordinatorInstallEnvironment,
  coordinatorRepoFromService,
  COORDINATOR_DEPLOY_JOURNAL_SCHEMA_VERSION,
  parseCoordinatorDeployJournal,
  writeCoordinatorDeployJournal,
  writeCoordinatorDeployPhase,
  type CoordinatorDeployJournalContext,
  type CoordinatorDeployJournalV1,
} from "./coordinator-deploy-journal.ts";
import {
  VERIFY_POLL_MS,
  VERIFY_TIMEOUT_MS,
  commitCoordinatorDeploy,
  coordinatorReportIsHealthy,
  coordinatorReportIsOperational,
  coordinatorStartupPolicyIsEnabled,
  currentServiceTargetsRelease,
  flushCoordinatorReleaseTree,
  recoverCoordinatorDeploy,
  removeStagedCoordinatorRelease,
  restorePriorCoordinator,
} from "./coordinator-deploy-recovery.ts";
import { normalizedHost, tryCoordinatorSelfUpdate } from "./deploy-windows-channel.ts";

const REPO_ROOT = resolve(import.meta.dir, "..", "..", "..");
function hostLabel(value: string): string {
  return normalizedHost(value).split(".")[0] ?? "";
}

function assertSafeSshTarget(value: string): string {
  const target = value.trim();
  if (!/^[A-Za-z0-9][A-Za-z0-9._:-]*$/.test(target)) {
    throw new Error(`invalid SSH deployment target: ${JSON.stringify(value)}`);
  }
  return target;
}

export function resolvePushTargets(
  configured: string | undefined,
  workers: readonly WorkerStatus[],
): string[] {
  const requested = (configured ?? "").split(",").map((value) => value.trim()).filter(Boolean);
  const source = requested.length > 0
    ? requested
    : workers.map((worker) => worker.os === "win32"
      ? worker.fingerprint
      : (worker.reachableAddr || worker.label));
  const seen = new Set<string>();
  const targets: string[] = [];
  for (const value of source) {
    const resolved = resolveWorkerTarget(workers, value);
    const canonical = resolved.worker
      ? (resolved.worker.os === "win32"
        ? resolved.worker.fingerprint
        : (resolved.worker.reachableAddr || resolved.worker.label))
      : value;
    const target = assertSafeSshTarget(canonical);
    const key = normalizedHost(target);
    if (!key || seen.has(key)) continue;
    seen.add(key);
    targets.push(target);
  }
  return targets;
}


function resolveWorkerTarget(
  workers: readonly WorkerStatus[],
  target: string,
): { worker: WorkerStatus | null; ambiguous: boolean } {
  const normalizedTarget = normalizedHost(target);
  const fingerprintMatches = workers.filter(
    (worker) => normalizedHost(worker.fingerprint) === normalizedTarget,
  );
  if (fingerprintMatches.length > 0) {
    return {
      worker: fingerprintMatches.length === 1 ? fingerprintMatches[0] : null,
      ambiguous: fingerprintMatches.length > 1,
    };
  }
  const exact = workers.filter((worker) =>
    [worker.label, worker.reachableAddr]
      .filter((value): value is string => Boolean(value))
      .some((value) => normalizedHost(value) === normalizedTarget)
  );
  if (exact.length > 0) {
    return { worker: exact.length === 1 ? exact[0] : null, ambiguous: exact.length > 1 };
  }
  const targetLabel = hostLabel(normalizedTarget);
  const aliases = workers.filter((worker) =>
    [worker.label, worker.reachableAddr]
      .filter((value): value is string => Boolean(value))
      .some((value) => hostLabel(normalizedHost(value)) === targetLabel)
  );
  return { worker: aliases.length === 1 ? aliases[0] : null, ambiguous: aliases.length > 1 };
}

export async function preflightWindowsFleetRelease(
  expectedSha: string,
  fetchImpl: typeof fetch = fetch,
): Promise<{ manifestSha256: string }> {
  const release = await fetchAndVerifyReleaseAsset(WINDOWS_RELEASE_MANIFEST_ASSET, {
    fetchImpl,
    subject: "Windows fleet manifest",
    timeoutMs: 30_000,
    checksumTimeoutMs: 30_000,
    fail: (message) => new DeployFailure(8, message),
  });
  const manifest = parseWindowsReleaseManifest(release.bytes);
  if (manifest.build !== expectedSha) {
    throw new DeployFailure(
      8,
      `Windows release manifest reports ${manifest.build}, expected source commit ${expectedSha}`,
    );
  }
  return { manifestSha256: release.sha256 };
}

export function ambiguousPushTargets(
  targets: readonly string[],
  workers: readonly WorkerStatus[],
): string[] {
  return targets.filter((target) => resolveWorkerTarget(workers, target).ambiguous);
}


export function workerVersionProblems(
  targets: readonly string[],
  workers: readonly WorkerStatus[],
  expectedSha: string,
  deployedAfter: ReadonlyMap<string, number> = new Map(),
): string[] {
  const problems: string[] = [];
  for (const target of targets) {
    const resolved = resolveWorkerTarget(workers, target);
    const worker = resolved.worker;
    if (resolved.ambiguous) {
      problems.push(`${target}: ambiguous coordinator worker identity`);
    } else if (!worker) {
      problems.push(`${target}: missing from coordinator worker inventory`);
    } else if (worker.stale) {
      problems.push(`${worker.label}: stale`);
    } else if (worker.gitSha !== expectedSha) {
      problems.push(`${worker.label}: reports ${worker.gitSha ?? "no SHA"}, expected ${expectedSha}`);
    } else if (worker.keeperState === "unknown") {
      problems.push(`${worker.label}: keeper build proof is unavailable`);
    } else if (worker.keeperState === "stale") {
      problems.push(`${worker.label}: keeper reports stale build ${worker.keeperBuild ?? "unknown"}`);
    } else if (worker.lastSeenMs <= (deployedAfter.get(normalizedHost(target)) ?? 0)) {
      problems.push(`${worker.label}: awaiting a post-deploy heartbeat`);
    }
  }
  return problems;
}


function hasWebDist(path: string): boolean {
  return existsSync(join(path, "index.html"));
}

export function preserveWebDistForNoBuild(
  releaseDir: string,
  installedEnvironment: Readonly<Record<string, string>>,
  priorRepo: string,
): string {
  const destination = join(releaseDir, "apps", "web", "dist");
  if (hasWebDist(destination)) return destination;
  const configured = installedEnvironment.ROOST_WEB_DIST_PATH;
  for (const candidate of [
    configured ? (isAbsolute(configured) ? configured : resolve(priorRepo, configured)) : undefined,
    join(priorRepo, "apps", "web", "dist"),
  ]) {
    if (!candidate || resolve(candidate) === resolve(destination) || !hasWebDist(candidate)) continue;
    mkdirSync(dirname(destination), { recursive: true });
    cpSync(candidate, destination, { recursive: true, dereference: true });
    return destination;
  }
  throw new DeployFailure(
    5,
    "--no-web requested but neither the staged commit nor the installed coordinator has apps/web/dist",
  );
}

export function workerConvergenceThresholds(
  targets: readonly string[],
  activatedAt: number,
): Map<string, number> {
  return new Map(targets.map((target) => [normalizedHost(target), activatedAt]));
}


function resolveCoordinatorRepo(): string {
  if (process.platform !== "linux" && process.platform !== "darwin") {
    failDeploy(2, "roost push updates source-installed POSIX coordinators; use `roost update` on Windows");
  }
  const override = process.env.ROOST_COORD_REPO_DIR?.trim();
  let installed: string | null = null;
  const servicePath = coordServicePath();
  if (existsSync(servicePath)) {
    installed = coordinatorRepoFromService(readFileSync(servicePath, "utf8"), process.platform);
  }
  for (const candidate of [override, installed, REPO_ROOT]) {
    if (!candidate) continue;
    const absolute = resolve(candidate);
    if (existsSync(join(absolute, ".git"))
      && existsSync(join(absolute, "apps", "coord", "scripts", "install.sh"))) {
      return realpathSync(absolute);
    }
  }
  failDeploy(
    2,
    `cannot locate the coordinator source checkout from ${servicePath}; set ROOST_COORD_REPO_DIR`,
  );
}

export function foreignWorkerDeployJournalForCoordinator(serviceRoot: string): string | null {
  for (const relative of [
    POSIX_WORKER_DEPLOY_JOURNAL_PATHS.local,
    POSIX_WORKER_DEPLOY_JOURNAL_PATHS.linux,
    POSIX_WORKER_DEPLOY_JOURNAL_PATHS.darwin,
  ]) {
    const candidate = join(serviceRoot, relative);
    try {
      lstatSync(candidate);
      return candidate;
    } catch (error) {
      if (!(error instanceof Error
        && "code" in error
        && (error as NodeJS.ErrnoException).code === "ENOENT")) {
        throw error;
      }
    }
  }
  return null;
}

async function deployLocalCoordinator(expectedSha: string, buildWeb: boolean): Promise<void> {
  if (process.platform !== "linux" && process.platform !== "darwin") {
    failDeploy(2, "roost push updates source-installed POSIX coordinators; use `roost update` on Windows");
  }
  const platform = process.platform;
  const configuredServiceRoot = resolve(roostServiceDir());
  mkdirSync(configuredServiceRoot, { recursive: true, mode: 0o700 });
  const serviceRoot = realpathSync(configuredServiceRoot);
  const releaseRoot = join(serviceRoot, "releases", "coord");
  const journalPath = join(serviceRoot, "transactions", "coordinator-deploy.json");
  const servicePath = resolve(coordServicePath());
  mkdirSync(releaseRoot, { recursive: true, mode: 0o700 });
  mkdirSync(dirname(journalPath), { recursive: true, mode: 0o700 });
  if (realpathSync(releaseRoot) !== releaseRoot
    || realpathSync(dirname(journalPath)) !== dirname(journalPath)) {
    failDeploy(5, "coordinator deployment directories must not traverse symbolic links");
  }

  const foreignJournal = foreignWorkerDeployJournalForCoordinator(serviceRoot);
  if (foreignJournal !== null) {
    failDeploy(5, `cannot mutate past unsettled foreign worker deploy journal: ${foreignJournal}`);
  }
  const transaction = await acquireMachineTransaction("deploy", journalPath);
  try {
    const journalContext: CoordinatorDeployJournalContext = {
      servicePath,
      releaseRoot,
      platform,
    };
    await recoverCoordinatorDeploy(journalPath, journalContext);

    const coordinatorRepo = resolveCoordinatorRepo();
    const releaseId = `${expectedSha}-${crypto.randomUUID()}`;
    const releaseDir = join(releaseRoot, releaseId);
    await runOrDie(["git", "fetch", "--quiet", "origin"], "coordinator git fetch", { cwd: coordinatorRepo, echo: true });

    if (!existsSync(servicePath)) {
      failDeploy(5, `coordinator service definition is missing: ${servicePath}`);
    }
    const priorDefinition = readFileSync(servicePath);
    const priorDefinitionMode = lstatSync(servicePath).mode & 0o777;
    const priorDefinitionText = priorDefinition.toString("utf8");
    const installedEnvironment = coordinatorInstallEnvironment(priorDefinitionText, platform);
    const priorSha = installedEnvironment.ROOST_GIT_SHA ?? installedEnvironment.GIT_SHA;
    if (!priorSha) {
      failDeploy(5, "coordinator service definition does not contain a prior Git SHA");
    }
    const priorReport = await statusReport();
    if (!coordinatorReportIsHealthy(priorReport, priorSha)
      || !await coordinatorStartupPolicyIsEnabled(platform)) {
      failDeploy(
        5,
        `coordinator must be running at its recorded prior build ${priorSha} with automatic startup before update`,
      );
    }
    const sourceReleasePath = coordinatorRepoFromService(priorDefinitionText, platform);
    if (!sourceReleasePath) {
      failDeploy(5, "coordinator service definition does not contain a WorkingDirectory");
    }
    const priorEnvironment: Record<string, string> = {
      ...installedEnvironment,
      GIT_SHA: expectedSha,
      ROOST_GIT_SHA: expectedSha,
      ROOST_WORKDIR: releaseDir,
      ROOST_EXEC_BIN: "",
      ROOST_REPO_ROOT: releaseDir,
      ROOST_SKIP_ENV_LOCAL: "1",
      ROOST_WEB_DIST_PATH: join(releaseDir, "apps", "web", "dist"),
      ...(platform === "linux"
        ? { ROOST_COORD_UNIT: servicePath }
        : { ROOST_COORD_PLIST: servicePath }),
    };
    let journal = parseCoordinatorDeployJournal(
      JSON.stringify({
        schemaVersion: COORDINATOR_DEPLOY_JOURNAL_SCHEMA_VERSION,
        phase: "prepared",
        priorDefinitionMode,
        priorDefinitionBase64: priorDefinition.toString("base64"),
        priorSha,
        targetSha: expectedSha,
        servicePath,
        sourceReleasePath,
        stagingRepoPath: coordinatorRepo,
        stagedReleasePath: releaseDir,
      }),
      journalContext,
    );
    await writeCoordinatorDeployJournal(journalPath, journal);

    console.log(`\n>> stage coordinator release ${releaseDir}`);
    try {
      await runOrDie(
        ["git", "worktree", "add", "--quiet", "--force", "--detach", releaseDir, expectedSha],
        "coordinator worktree stage",
        { cwd: coordinatorRepo, echo: true },
      );
      await runOrDie(["bun", "install", "--frozen-lockfile"], "coordinator frozen bun install", { cwd: releaseDir, echo: true });
      if (buildWeb) {
        console.log(`\n>> build coordinator SPA (${releaseDir}/apps/web → dist)`);
        await runOrDie(["bun", "run", "build"], "coordinator SPA build", { cwd: join(releaseDir, "apps", "web"), echo: true });
      } else {
        preserveWebDistForNoBuild(releaseDir, installedEnvironment, sourceReleasePath);
      }
      await flushCoordinatorReleaseTree(releaseRoot, releaseDir, expectedSha);
    } catch (error) {
      try {
        await removeStagedCoordinatorRelease(
          releaseRoot,
          coordinatorRepo,
          releaseDir,
          expectedSha,
        );
        await durableRemove(journalPath);
      } catch (cleanupError) {
        throw new DeployFailure(
          error instanceof DeployFailure ? error.exitCode : 5,
          `${error instanceof Error ? error.message : String(error)}\n` +
            `coordinator prepared-stage cleanup failed: ${
              cleanupError instanceof Error ? cleanupError.message : String(cleanupError)
            }`,
        );
      }
      throw error;
    }

    console.log("\n>> activate and restart local coordinator");
    try {
      journal = await writeCoordinatorDeployPhase(journalPath, journal, "activating");
      await runOrDie(
        ["bash", "apps/coord/scripts/install.sh", "install"],
        "coordinator install",
        { cwd: releaseDir, echo: true },
      );
    } catch (error) {
      const rollbackError = await restorePriorCoordinator(journalPath, releaseRoot, journal);
      throw new DeployFailure(
        5,
        `${error instanceof Error ? error.message : String(error)}\n` +
          (rollbackError ?? "prior coordinator service restored"),
      );
    }

    const deadline = Date.now() + VERIFY_TIMEOUT_MS;
    for (;;) {
      let report: StatusReport;
      try {
        report = await statusReport();
      } catch (error) {
        const rollbackError = await restorePriorCoordinator(journalPath, releaseRoot, journal);
        throw new DeployFailure(
          8,
          `coordinator verification failed: ${error instanceof Error ? error.message : String(error)}\n` +
            (rollbackError ?? "prior coordinator service restored"),
        );
      }
      if (currentServiceTargetsRelease(journal, platform)
        && coordinatorReportIsHealthy(report, expectedSha)) {
        await commitCoordinatorDeploy(journalPath, releaseRoot, journal);
        return;
      }
      if (Date.now() >= deadline) {
        const rollbackError = await restorePriorCoordinator(journalPath, releaseRoot, journal);
        failDeploy(
          8,
          `coordinator did not become healthy at ${expectedSha}; ` +
            `reported ${report.coord.gitSha ?? "no SHA"}, TLS=${report.tlsMode}\n` +
            (rollbackError ?? "prior coordinator service restored"),
        );
      }
      await Bun.sleep(VERIFY_POLL_MS);
    }
  } finally {
    await transaction.release();
  }
}

async function verifyWorkerVersions(
  targets: readonly string[],
  expectedSha: string,
  deployedAfter: ReadonlyMap<string, number>,
): Promise<void> {
  const deadline = Date.now() + VERIFY_TIMEOUT_MS;
  let problems: string[] = [];
  do {
    problems = workerVersionProblems(targets, workerInventory(), expectedSha, deployedAfter);
    if (problems.length === 0) return;
    await Bun.sleep(VERIFY_POLL_MS);
  } while (Date.now() < deadline);
  failDeploy(8, `workers did not converge on ${expectedSha}:\n${problems.join("\n")}`);
}

export async function deployCoordinatorForPlatform(
  expectedSha: string,
  buildWeb: boolean,
  deps: {
    windows?: (sha: string) => Promise<boolean | null>;
    posix?: (sha: string, build: boolean) => Promise<void>;
  } = {},
): Promise<"windows" | "posix"> {
  const windowsUpdated = await (deps.windows ?? tryCoordinatorSelfUpdate)(expectedSha);
  if (windowsUpdated !== null) return "windows";
  await (deps.posix ?? deployLocalCoordinator)(expectedSha, buildWeb);
  return "posix";
}

export async function push(args: string[]): Promise<void> {
  const skipGit = args.includes("--no-git");
  const skipLocalCoord = args.includes("--no-coord");
  const targetsArg = args.find((arg) => arg.startsWith("--targets="));
  const inventory = workerInventory();
  const configured = targetsArg?.slice("--targets=".length) ?? process.env.ROOST_PUSH_TARGETS;
  const targets = resolvePushTargets(configured, inventory);
  const ambiguousTargets = ambiguousPushTargets(targets, inventory);
  if (ambiguousTargets.length > 0) {
    failDeploy(
      2,
      `ambiguous push targets: ${ambiguousTargets.join(", ")}; use exact full worker labels or addresses`,
    );
  }
  if (targets.length === 0) {
    failDeploy(
      2,
      "no push targets: register a worker, set ROOST_PUSH_TARGETS, or pass --targets=host1,host2",
    );
  }

  const expectedSha = resolveLocalGitShaOrDie(REPO_ROOT);
  if (!expectedSha || expectedSha.endsWith("-dirty")) {
    failDeploy(7, "roost push requires a clean commit");
  }

  const publishTarget = resolveGitPublishTargetOrDie(REPO_ROOT);
  if (!skipGit) {
    console.log(`>> git push ${publishTarget.remote} HEAD:${publishTarget.mergeRef}`);
    await runOrDie(
      ["git", "push", "--", publishTarget.remote, `HEAD:${publishTarget.mergeRef}`],
      "git push",
      { cwd: REPO_ROOT, echo: true },
    );
  } else {
    console.log(">> skipping git transmission (--no-git); verifying remote identity");
  }
  resolvePublishedGitShaOrDie(REPO_ROOT, expectedSha);
  const targetsWindows = targets.some((target) =>
    resolveWorkerTarget(inventory, target).worker?.os === "win32"
  );
  const windowsRelease = targetsWindows
    ? await preflightWindowsFleetRelease(expectedSha)
    : null;
  const initialCoordinator = await statusReport();
  if (!coordinatorReportIsOperational(initialCoordinator)) {
    failDeploy(8, "coordinator must be running and healthy before fleet mutation begins");
  }

  if (skipLocalCoord) {
    if (!coordinatorReportIsHealthy(initialCoordinator, expectedSha)) {
      failDeploy(
        8,
        "--no-coord requires a live coordinator already reporting the exact target build",
      );
    }
    console.log("\n>> skipping local coordinator update (--no-coord)");
  } else {
    // Activate the coordinator first so every Windows START RPC understands
    // and enforces the preflight build/digest pins. Its own transaction proves
    // health or rolls back before any remote worker mutates.
    await deployCoordinatorForPlatform(expectedSha, !args.includes("--no-web"));
  }

  console.log(`>> fleet targets: ${targets.join(", ")}`);
  const deployedAfter = new Map<string, number>();
  const outcomes = await Promise.all(targets.map(async (host) => {
    console.log(`\n>> roost deploy ${host}`);
    deployedAfter.set(normalizedHost(host), Date.now());
    try {
      await deploy([
        host,
        `--source-root=${REPO_ROOT}`,
        `--expected-sha=${expectedSha}`,
        ...(windowsRelease
          ? [`--expected-manifest-sha256=${windowsRelease.manifestSha256}`]
          : []),
      ]);
      return { host, error: null };
    } catch (error) {
      return {
        host,
        error: error instanceof Error ? error.message : String(error),
      };
    }
  }));
  const deployed = outcomes.filter(({ error }) => error === null).map(({ host }) => host);
  const failures = outcomes
    .filter(({ error }) => error !== null)
    .map(({ host, error }) => `${host}: ${error}`);
  for (const { host, error } of outcomes) {
    if (error !== null) console.error(`deploy ${host} failed after independent fleet dispatch`);
  }



  try {
    await verifyWorkerVersions(deployed, expectedSha, deployedAfter);
  } catch (error) {
    failures.push(error instanceof Error ? error.message : String(error));
  }
  const finalCoordinator = await statusReport();
  if (!coordinatorReportIsHealthy(finalCoordinator, expectedSha)) {
    failures.push(`coordinator is not live on exact build ${expectedSha} after worker rollout`);
  }

  if (failures.length > 0) {
    failDeploy(1, `fleet push incomplete:\n${failures.join("\n")}`);
  }
  console.log(`\n>> push complete — coordinator and ${deployed.length} workers report ${expectedSha}`);
}
