import { win32 } from "node:path";
import { acquireMachineTransaction } from "../machine-transaction.ts";
import { roostServiceDir, roostVersionsDir } from "@roost/shared/paths";
import {
  windowsCoordinatorRelocationState,
  windowsProtectUpdaterArtifact,
  windowsQueryService,
  windowsReadUpdaterArtifact,
  windowsRemoveUpdaterArtifact,
  windowsReplaceUpdaterArtifact,
  windowsStartService,
  windowsStopService,
  type WindowsServiceSnapshot,
} from "@roost/shared/windows-helper";
import {
  WINDOWS_RELOCATION_SCHEMA_VERSION,
  windowsRelocationOverridePath,
  type WindowsCoordinatorPromotionRelocationOperation,
  type WindowsRelocationBrokerCommand,
  type WindowsRelocationOperationKind,
  type WindowsRelocationOperation,
  type WindowsRelocationRoleOverride,
} from "@roost/shared/windows-relocation";
import {
  WINDOWS_SERVICE_NAMES,
  WINDOWS_SERVICE_ROLES,
  loadWindowsServiceDefinitions,
  quoteWindowsArg,
  windowsServiceDefinitionsPath,
  type RoostServiceRole,
  type WindowsServiceDefinition,
} from "../service-ctl.ts";
import {
  DurableWindowsRelocationJournalStore,
  windowsRelocationJournalPath,
  type WindowsRelocationJournalStore,
  type WindowsRelocationJournalV1,
} from "./windows-relocation-journal.ts";
import type { WindowsUpdateNative } from "./windows-update-broker.ts";
import { normalizedWindowsAccount, runTrustedTailscale } from "./windows-identity.ts";
import {
  assertNoReparseComponents,
  depsNow,
  errorText,
  samePath,
} from "./windows-path-safety.ts";
import { createWindowsUpdateNative } from "./windows-update-runtime.ts";

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const GIT_SHA_RE = /^[0-9a-f]{40,64}$/i;
const MAX_OVERRIDE_BYTES = 32 * 1024;

export interface WindowsRelocationBrokerDeps {
  store: WindowsRelocationJournalStore;
  native: Pick<WindowsUpdateNative, "assertUpdaterServiceContext">;
  serviceDir: string;
  versionsDir: string;
  loadDefinitions: typeof loadWindowsServiceDefinitions;
  queryService: typeof windowsQueryService;
  startService: typeof windowsStartService;
  stopService: typeof windowsStopService;
  relocationState: typeof windowsCoordinatorRelocationState;
  acquireTransaction: typeof acquireMachineTransaction;
  captureTailscale(relocationId: string): Promise<string>;
  applyTailscale(): Promise<void>;
  restoreTailscale(relocationId: string, prior: string): Promise<void>;
  coordinatorHealthy(targetUrl: string): Promise<boolean>;
  sleep?: (milliseconds: number) => Promise<void>;
  now?: () => Date;
}

export function createWindowsRelocationBrokerDeps(
  operationKind: WindowsRelocationOperationKind = "worker-endpoint",
): WindowsRelocationBrokerDeps {
  const serviceDir = roostServiceDir();
  return {
    store: new DurableWindowsRelocationJournalStore(
      windowsRelocationJournalPath(serviceDir, operationKind),
    ),
    native: createWindowsUpdateNative(),
    serviceDir,
    versionsDir: roostVersionsDir(),
    loadDefinitions: loadWindowsServiceDefinitions,
    queryService: windowsQueryService,
    startService: windowsStartService,
    stopService: windowsStopService,
    relocationState: windowsCoordinatorRelocationState,
    acquireTransaction: acquireMachineTransaction,
    captureTailscale: (relocationId) => captureTailscale(serviceDir, relocationId),
    applyTailscale,
    restoreTailscale: (relocationId, prior) => restoreTailscale(serviceDir, relocationId, prior),
    coordinatorHealthy,
  };
}

/** Validate and snapshot before the Worker is allowed to stop a service. */
export async function prepareWindowsRelocationJournal(
  command: WindowsRelocationBrokerCommand,
  admissionPath: string,
  deps: WindowsRelocationBrokerDeps,
): Promise<WindowsRelocationJournalV1> {
  if (command.action !== "START" || !command.operation) throw new Error("relocation preparation requires START");
  validateWindowsRelocationCommand(command);
  await validateOperationAgainstMachine(command.operation, deps, true);
  if (command.operation.kind === "coordinator-promotion") {
    await deps.relocationState(
      "admit-stage",
      command.operation.relocationId,
      command.operation.handoffId,
    );
  }
  const timestamp = depsNow(deps).toISOString();
  return {
    schemaVersion: WINDOWS_RELOCATION_SCHEMA_VERSION,
    relocationId: command.relocationId,
    handoffId: command.handoffId,
    operationKind: command.operationKind,
    operation: command.operation,
    phase: "prepared",
    revision: 1,
    priorOverrideRaw: await readOptionalOverride(
      windowsRelocationOverridePath(deps.serviceDir, overrideRole(command.operation)),
    ),
    desiredOverride: desiredRoleOverride(command.operation),
    admissionPath,
    result: {
      action: "START",
      revision: 1,
      success: true,
      message: "relocation validated and prepared before lifecycle mutation",
      completedAt: timestamp,
    },
    createdAt: timestamp,
    updatedAt: timestamp,
  };
}

/** Persist a START rejection so the unprivileged caller observes a durable
 * terminal failure instead of timing out. No override or lifecycle mutation
 * has occurred when this constructor is used. */
export function rejectedWindowsRelocationJournal(
  command: WindowsRelocationBrokerCommand,
  admissionPath: string,
  error: unknown,
  deps: Pick<WindowsRelocationBrokerDeps, "now">,
): WindowsRelocationJournalV1 {
  if (command.action !== "START" || !command.operation) throw new Error("rejected relocation has no START operation");
  const timestamp = depsNow(deps).toISOString();
  return {
    schemaVersion: WINDOWS_RELOCATION_SCHEMA_VERSION,
    relocationId: command.relocationId,
    handoffId: command.handoffId,
    operationKind: command.operationKind,
    operation: command.operation,
    phase: "rolled-back",
    revision: 1,
    priorOverrideRaw: null,
    desiredOverride: desiredRoleOverride(command.operation),
    admissionPath,
    result: {
      action: "START",
      revision: 1,
      success: false,
      message: "relocation admission rejected before lifecycle mutation",
      error: errorText(error),
      completedAt: timestamp,
    },
    createdAt: timestamp,
    updatedAt: timestamp,
  };
}

/** RoostUpdaterV2 owns one lease from first mutation through COMMIT/RESTORE. */
export async function runWindowsRelocationBroker(
  deps: WindowsRelocationBrokerDeps = createWindowsRelocationBrokerDeps(),
): Promise<{ handled: boolean; journal: WindowsRelocationJournalV1 | null }> {
  await deps.native.assertUpdaterServiceContext();
  let journal = await deps.store.load();
  if (!journal) return { handled: false, journal: null };
  if (journal.phase === "prepared" || journal.phase === "committed" || journal.phase === "rolled-back") {
    return { handled: true, journal };
  }
  const lock = await deps.acquireTransaction("relocation", deps.store.path, { platform: "win32" });
  try {
    for (;;) {
      journal = await deps.store.load();
      if (!journal) throw new Error("durable relocation disappeared while its lease was held");
      try {
        journal = await processJournal(journal, deps);
      } catch (error) {
        journal = await restoreTransaction(journal, deps, "APPLY", errorText(error));
      }
      if (journal.phase === "committed" || journal.phase === "rolled-back") {
        return { handled: true, journal };
      }
      if (journal.phase !== "applied") continue;
      const { admitPendingWindowsRelocationRequest } = await import("./windows-relocation-control.ts");
      const admitted = await admitPendingWindowsRelocationRequest().catch(() => null);
      if (!admitted) await (deps.sleep ?? Bun.sleep)(100);
    }
  } finally {
    await lock.release();
  }
}

async function processJournal(
  journal: WindowsRelocationJournalV1,
  deps: WindowsRelocationBrokerDeps,
): Promise<WindowsRelocationJournalV1> {
  if (journal.phase === "apply-requested" || journal.phase === "applying") {
    if (journal.phase === "apply-requested") {
      journal = await saveCheckpoint({ ...journal, phase: "applying" }, deps);
    }
    if (!journal.coordinator) {
      await validateOperationAgainstMachine(journal.operation, deps, true);
    }
    if (journal.operation.kind === "coordinator-promotion") {
      journal = await applyCoordinator(journal, deps);
    } else {
      await replaceControlText(
        windowsRelocationOverridePath(deps.serviceDir, overrideRole(journal.operation)),
        desiredOverrideRaw(journal),
      );
    }
    await assertOverride(journal, desiredOverrideRaw(journal), deps.serviceDir);
    return await terminalCheckpoint(journal, "applied", "APPLY", true, "relocation applied and health-proven", deps);
  }
  if (journal.phase === "commit-requested") {
    if (journal.operation.kind === "coordinator-promotion") {
      await deps.relocationState("commit", journal.relocationId, journal.handoffId);
      journal = await saveCheckpoint({
        ...journal,
        coordinator: { ...journal.coordinator!, phase: "committed" },
      }, deps);
    }
    await assertOverride(journal, desiredOverrideRaw(journal), deps.serviceDir);
    return await terminalCheckpoint(journal, "committed", "COMMIT", true, "relocation committed", deps);
  }
  if (journal.phase === "restore-requested" || journal.phase === "restoring") {
    return await restoreTransaction(journal, deps, "RESTORE");
  }
  return journal;
}

async function applyCoordinator(
  journal: WindowsRelocationJournalV1,
  deps: WindowsRelocationBrokerDeps,
): Promise<WindowsRelocationJournalV1> {
  const operation = journal.operation as WindowsCoordinatorPromotionRelocationOperation;
  let checkpoint = journal.coordinator;
  if (!checkpoint) {
    await deps.relocationState("prepare", journal.relocationId, journal.handoffId);
    checkpoint = {
      phase: "captured",
      priorCoordinatorRunning: operation.expectedBefore.state === "running",
      priorTailscaleConfig: await deps.captureTailscale(journal.relocationId),
      rollbackPrepared: true,
    };
    journal = await saveCheckpoint({ ...journal, coordinator: checkpoint }, deps);
  }
  if (checkpoint.phase === "captured") {
    if (checkpoint.priorCoordinatorRunning) {
      await deps.stopService(WINDOWS_SERVICE_NAMES.coordinator, 30_000);
    }
    checkpoint = { ...checkpoint, phase: "coordinator-stopped" };
    journal = await saveCheckpoint({ ...journal, coordinator: checkpoint }, deps);
  }
  if (checkpoint.phase === "coordinator-stopped") {
    await deps.relocationState("promote", journal.relocationId, journal.handoffId);
    checkpoint = { ...checkpoint, phase: "state-promoted" };
    journal = await saveCheckpoint({ ...journal, coordinator: checkpoint }, deps);
  }
  if (checkpoint.phase === "state-promoted") {
    await replaceControlText(
      windowsRelocationOverridePath(deps.serviceDir, "coordinator"),
      desiredOverrideRaw(journal),
    );
    checkpoint = { ...checkpoint, phase: "override-applied" };
    journal = await saveCheckpoint({ ...journal, coordinator: checkpoint }, deps);
  }
  if (checkpoint.phase === "override-applied") {
    await deps.applyTailscale();
    checkpoint = { ...checkpoint, phase: "route-applied" };
    journal = await saveCheckpoint({ ...journal, coordinator: checkpoint }, deps);
  }
  if (checkpoint.phase === "route-applied") {
    await deps.startService(WINDOWS_SERVICE_NAMES.coordinator);
    checkpoint = { ...checkpoint, phase: "coordinator-started" };
    journal = await saveCheckpoint({ ...journal, coordinator: checkpoint }, deps);
  }
  if (checkpoint.phase === "coordinator-started") {
    let healthy = false;
    for (let attempt = 0; attempt < 5 && !healthy; ++attempt) {
      if (attempt > 0) await (deps.sleep ?? Bun.sleep)(2_000);
      healthy = await deps.coordinatorHealthy(operation.targetUrl);
    }
    if (!healthy) throw new Error("relocated coordinator did not pass health proof");
    checkpoint = { ...checkpoint, phase: "healthy" };
    journal = await saveCheckpoint({ ...journal, coordinator: checkpoint }, deps);
  }
  return journal;
}

async function restoreTransaction(
  journal: WindowsRelocationJournalV1,
  deps: WindowsRelocationBrokerDeps,
  resultAction: "APPLY" | "RESTORE",
  error?: string,
): Promise<WindowsRelocationJournalV1> {
  if (journal.phase !== "restoring") {
    journal = await saveCheckpoint({ ...journal, phase: "restoring" }, deps);
  }
  if (journal.operation.kind === "coordinator-promotion" && journal.coordinator?.rollbackPrepared) {
    const service = await deps.queryService(WINDOWS_SERVICE_NAMES.coordinator);
    if (service.state !== "stopped") {
      await deps.stopService(WINDOWS_SERVICE_NAMES.coordinator, 30_000);
    }
    await deps.relocationState("restore", journal.relocationId, journal.handoffId);
    await deps.restoreTailscale(journal.relocationId, journal.coordinator.priorTailscaleConfig);
  }
  await restorePriorOverride(journal, deps.serviceDir);
  await assertOverride(journal, journal.priorOverrideRaw, deps.serviceDir);
  if (journal.operation.kind === "coordinator-promotion" && journal.coordinator?.priorCoordinatorRunning) {
    await deps.startService(WINDOWS_SERVICE_NAMES.coordinator);
  }
  if (journal.coordinator) {
    journal = await saveCheckpoint({
      ...journal,
      coordinator: { ...journal.coordinator, phase: "restored" },
    }, deps);
  }
  return await terminalCheckpoint(
    journal,
    "rolled-back",
    resultAction,
    error === undefined,
    error ? "relocation restored after apply failure" : "relocation restored",
    deps,
    error,
  );
}

async function restorePriorOverride(journal: WindowsRelocationJournalV1, serviceDir: string): Promise<void> {
  const path = windowsRelocationOverridePath(serviceDir, overrideRole(journal.operation));
  if (journal.priorOverrideRaw === null) {
    await windowsRemoveUpdaterArtifact(path, "control");
    return;
  }
  await replaceControlText(path, journal.priorOverrideRaw);
}

async function replaceControlText(path: string, contents: string): Promise<void> {
  await windowsReplaceUpdaterArtifact(path, "control", new TextEncoder().encode(contents));
}

async function assertOverride(
  journal: WindowsRelocationJournalV1,
  expected: string | null,
  serviceDir: string,
): Promise<void> {
  const actual = await readOptionalOverride(
    windowsRelocationOverridePath(serviceDir, overrideRole(journal.operation)),
  );
  if (actual !== expected) throw new Error("relocation override did not round-trip exactly");
}

async function terminalCheckpoint(
  journal: WindowsRelocationJournalV1,
  phase: "applied" | "committed" | "rolled-back",
  action: "APPLY" | "COMMIT" | "RESTORE",
  success: boolean,
  message: string,
  deps: WindowsRelocationBrokerDeps,
  error?: string,
): Promise<WindowsRelocationJournalV1> {
  const revision = journal.revision + 1;
  return await saveCheckpoint({
    ...journal,
    phase,
    revision,
    pendingAction: undefined,
    result: { action, revision, success, message, error, completedAt: depsNow(deps).toISOString() },
  }, deps, false);
}

async function saveCheckpoint(
  journal: WindowsRelocationJournalV1,
  deps: WindowsRelocationBrokerDeps,
  advance = true,
): Promise<WindowsRelocationJournalV1> {
  const next = {
    ...journal,
    revision: advance ? journal.revision + 1 : journal.revision,
    updatedAt: depsNow(deps).toISOString(),
  };
  await deps.store.save(next);
  return next;
}

export function validateWindowsRelocationCommand(command: WindowsRelocationBrokerCommand): void {
  const commandKeys = Object.keys(command).filter((key) => command[key as keyof WindowsRelocationBrokerCommand] !== undefined);
  const allowedCommandKeys = [
    "action", "afterRevision", "handoffId", "operation", "operationKind",
    "relocationId", "requestId", "schemaVersion",
  ];
  if (commandKeys.some((key) => !allowedCommandKeys.includes(key))) {
    throw new Error("Windows relocation command contains an unknown field");
  }
  if (
    command.schemaVersion !== WINDOWS_RELOCATION_SCHEMA_VERSION
    || !boundedIdentifier(command.requestId)
    || !UUID_RE.test(command.relocationId)
    || !UUID_RE.test(command.handoffId)
    || !["worker-endpoint", "coordinator-promotion"].includes(command.operationKind)
    || !["START", "APPLY", "STATUS", "COMMIT", "RESTORE"].includes(command.action)
    || (command.afterRevision !== undefined && (!Number.isSafeInteger(command.afterRevision) || command.afterRevision < 0))
  ) throw new Error("invalid Windows relocation broker command");
  if (command.action === "START") {
    if (!command.operation) throw new Error("relocation START requires an operation");
    validateOperationShape(command.operation);
    if (
      command.operation.relocationId !== command.relocationId
      || command.operation.handoffId !== command.handoffId
      || command.operation.kind !== command.operationKind
    ) throw new Error("relocation command is not bound to its operation");
  } else if (command.operation !== undefined) {
    throw new Error("only relocation START may carry an operation");
  }
}

function validateOperationShape(operation: WindowsRelocationOperation): void {
  if (operation.kind !== "worker-endpoint" && operation.kind !== "coordinator-promotion") {
    throw new Error("unknown Windows relocation operation");
  }
  const operationKeys = Object.keys(operation).sort();
  const allowedOperationKeys = (operation.kind === "worker-endpoint"
    ? ["expectedBefore", "handoffId", "kind", "relocationId", "schemaVersion", "sourceUrl", "targetUrl"]
    : [
      "expectedBefore", "expectedGitSha", "handoffId", "kind", "paths",
      "relocationId", "schemaVersion", "sourceUrl", "targetUrl",
    ]).sort();
  if (JSON.stringify(operationKeys) !== JSON.stringify(allowedOperationKeys)) {
    throw new Error("Windows relocation operation contains an unknown or missing field");
  }
  if (
    operation.schemaVersion !== WINDOWS_RELOCATION_SCHEMA_VERSION
    || !UUID_RE.test(operation.relocationId)
    || !UUID_RE.test(operation.handoffId)
    || !validCoordinatorUrl(operation.sourceUrl)
    || !validCoordinatorUrl(operation.targetUrl)
    || operation.sourceUrl === operation.targetUrl
    || !operation.expectedBefore
  ) throw new Error("invalid Windows relocation operation");
  const name = operation.kind === "worker-endpoint" ? WINDOWS_SERVICE_NAMES.worker : WINDOWS_SERVICE_NAMES.coordinator;
  if (operation.expectedBefore.name !== name) throw new Error("relocation targets a non-allowlisted service");
  if (operation.kind === "coordinator-promotion") {
    if (!GIT_SHA_RE.test(operation.expectedGitSha)) throw new Error("invalid relocation build identity");
    const keys = Object.keys(operation.paths).sort();
    const allowed = [
      "coordinatorAuthorizedKeysPath", "coordinatorDataDir", "coordinatorDbPath",
      "coordinatorHandoffPath", "coordinatorKeyPath", "coordinatorLogDir", "installRoot",
      "serviceDefinitionsPath", "serviceDir", "versionsDir",
    ].sort();
    if (JSON.stringify(keys) !== JSON.stringify(allowed)) throw new Error("invalid relocation path contract");
  }
}

async function validateOperationAgainstMachine(
  operation: WindowsRelocationOperation,
  deps: WindowsRelocationBrokerDeps,
  exactLifecycle: boolean,
): Promise<void> {
  validateOperationShape(operation);
  const definitions = await deps.loadDefinitions();
  assertStableDefinitions(definitions, deps.serviceDir, deps.versionsDir);
  if (operation.kind === "coordinator-promotion") assertCanonicalCoordinatorPaths(operation, deps);
  await assertNoReparseComponents(win32.dirname(deps.serviceDir), win32.join(win32.dirname(deps.serviceDir), "bin"));
  await assertNoReparseComponents(deps.serviceDir, deps.serviceDir);
  const role = operation.kind === "worker-endpoint" ? "worker" : "coordinator";
  const actual = await deps.queryService(WINDOWS_SERVICE_NAMES[role]);
  assertStableSnapshot(role, definitions[role], actual);
  if (!sameSnapshot(actual, operation.expectedBefore, exactLifecycle)) {
    throw new Error(`${actual.name} exact before-state changed before relocation admission`);
  }
  if (role === "worker" && actual.state !== "running") throw new Error("RoostWorkerV2 must be running");
  if (!exactLifecycle && role === "coordinator" && actual.state !== "stopped") {
    throw new Error("RoostCoordinatorV2 must be stopped before override publication");
  }
}

function assertStableDefinitions(
  definitions: Readonly<Record<RoostServiceRole, WindowsServiceDefinition>>,
  serviceDir: string,
  versionsDir: string,
): void {
  const installRoot = win32.dirname(serviceDir);
  const shawl = win32.join(installRoot, "bin", "shawl.exe");
  const launcher = win32.join(installRoot, "bin", "roost.exe");
  const args: Readonly<Record<RoostServiceRole, readonly string[]>> = {
    keeper: ["keeper", "--service"], worker: ["worker"], coordinator: ["coord"], updater: ["__windows-updater-broker"],
  };
  for (const role of WINDOWS_SERVICE_ROLES) {
    const definition = definitions[role];
    const separator = definition.shawlArguments.indexOf("--");
    const image = [definition.shawlPath, ...definition.shawlArguments].map(quoteWindowsArg).join(" ");
    if (
      definition.name !== WINDOWS_SERVICE_NAMES[role]
      || !samePath(definition.shawlPath, shawl)
      || !samePath(definition.serviceLauncherPath, launcher)
      || separator < 0
      || !samePath(definition.shawlArguments[separator + 1] ?? "", launcher)
      || definition.shawlArguments[separator + 2] !== "launch-current"
      || JSON.stringify(definition.shawlArguments.slice(separator + 3)) !== JSON.stringify(args[role])
      || definition.imagePath !== image
      || !samePath(definition.environment.ROOST_INSTALL_ROOT ?? "", installRoot)
      || !samePath(definition.environment.ROOST_SERVICE_DIR ?? "", serviceDir)
      || !samePath(definition.environment.ROOST_VERSIONS_DIR ?? "", versionsDir)
      || Object.keys(definition.environment).some((key) => key.toUpperCase() === "ROOST_WIN_HELPER")
    ) throw new Error(`${definition.name} is not an exact stable launch-current definition`);
  }
}

function assertStableSnapshot(
  role: RoostServiceRole,
  definition: WindowsServiceDefinition,
  snapshot: WindowsServiceSnapshot,
): void {
  const dependencies = definition.dependencies.map((entry) => WINDOWS_SERVICE_NAMES[entry].toLowerCase()).sort();
  if (
    snapshot.name !== WINDOWS_SERVICE_NAMES[role]
    || snapshot.imagePathRaw !== definition.imagePath
    || JSON.stringify(snapshot.binaryArgv) !== JSON.stringify([definition.shawlPath, ...definition.shawlArguments])
    || snapshot.startType !== definition.startMode
    || normalizedWindowsAccount(snapshot.account) !== normalizedWindowsAccount(definition.account)
    || JSON.stringify(snapshot.dependencies.map((entry) => entry.toLowerCase()).sort()) !== JSON.stringify(dependencies)
    || snapshot.displayName !== definition.displayName
    || snapshot.description !== definition.description
  ) throw new Error(`${snapshot.name} differs from its protected stable definition`);
}

function assertCanonicalCoordinatorPaths(
  operation: WindowsCoordinatorPromotionRelocationOperation,
  deps: Pick<WindowsRelocationBrokerDeps, "serviceDir" | "versionsDir">,
): void {
  const installRoot = win32.dirname(deps.serviceDir);
  const data = win32.join(deps.serviceDir, "data", "coordinator");
  const log = win32.join(deps.serviceDir, "logs", "coordinator");
  const expected = {
    installRoot, serviceDir: deps.serviceDir, versionsDir: deps.versionsDir,
    serviceDefinitionsPath: windowsServiceDefinitionsPath({ ROOST_SERVICE_DIR: deps.serviceDir }),
    coordinatorDataDir: data, coordinatorLogDir: log,
    coordinatorDbPath: win32.join(data, "coordinator_v2.db"),
    coordinatorKeyPath: win32.join(data, "ssh_ed25519.key"),
    coordinatorAuthorizedKeysPath: win32.join(data, "authorized_keys.roost"),
    coordinatorHandoffPath: win32.join(data, "coord-handoff.json"),
  };
  for (const [key, path] of Object.entries(expected)) {
    if (!win32.isAbsolute(path) || !samePath(operation.paths[key as keyof typeof expected], path)) {
      throw new Error(`coordinator relocation ${key} is not canonical`);
    }
  }
}

function desiredRoleOverride(operation: WindowsRelocationOperation): WindowsRelocationRoleOverride {
  const environment: Readonly<Record<string, string>> = operation.kind === "worker-endpoint" ? { ROOST_COORDINATOR_URL: operation.targetUrl } : {
    ROOST_COORD_DATA_DIR: operation.paths.coordinatorDataDir,
    ROOST_COORD_LOG_DIR: operation.paths.coordinatorLogDir,
    ROOST_COORDINATOR_DB: operation.paths.coordinatorDbPath,
    ROOST_COORDINATOR_KEY_PATH: operation.paths.coordinatorKeyPath,
    ROOST_COORDINATOR_AUTHORIZED_KEYS: operation.paths.coordinatorAuthorizedKeysPath,
    ROOST_COORDINATOR_HANDOFF_PATH: operation.paths.coordinatorHandoffPath,
    ROOST_COORDINATOR_PUBLIC_URL: operation.targetUrl,
    ROOST_GIT_SHA: operation.expectedGitSha.toLowerCase(),
    ROOST_SKIP_ENV_LOCAL: "1",
    ROOST_LOG_ENCODING: "utf-8",
  };
  return {
    schemaVersion: WINDOWS_RELOCATION_SCHEMA_VERSION,
    role: overrideRole(operation),
    relocationId: operation.relocationId,
    handoffId: operation.handoffId,
    environment,
  };
}

function desiredOverrideRaw(journal: WindowsRelocationJournalV1): string {
  return `${JSON.stringify(journal.desiredOverride)}\n`;
}

function overrideRole(operation: WindowsRelocationOperation): "worker" | "coordinator" {
  return operation.kind === "worker-endpoint" ? "worker" : "coordinator";
}

async function readOptionalOverride(path: string): Promise<string | null> {
  try {
    return new TextDecoder().decode(
      await windowsReadUpdaterArtifact(path, "control", MAX_OVERRIDE_BYTES),
    );
  } catch (error) {
    if (/\[win32=(?:2|3)\]/.test(String(error))) return null;
    throw error;
  }
}

function sameSnapshot(actual: WindowsServiceSnapshot, expected: WindowsServiceSnapshot, lifecycle: boolean): boolean {
  const project = (snapshot: WindowsServiceSnapshot): unknown => ({
    ...snapshot,
    pid: lifecycle ? snapshot.pid : undefined,
    state: lifecycle ? snapshot.state : undefined,
    dependencies: [...snapshot.dependencies].map((entry) => entry.toLowerCase()).sort(),
    environment: Object.fromEntries(Object.entries(snapshot.environment).sort(([a], [b]) => a.localeCompare(b))),
  });
  return JSON.stringify(project(actual)) === JSON.stringify(project(expected));
}

function validCoordinatorUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === "https:" && !url.username && !url.password && !url.search && !url.hash
      && url.pathname === "/" && url.port === "4102" && url.hostname.toLowerCase().endsWith(".ts.net");
  } catch { return false; }
}

function boundedIdentifier(value: string): boolean {
  return value.length > 0 && value.length <= 256 && !/[\0\r\n]/.test(value);
}

function tailscaleRollbackPath(serviceDir: string, relocationId: string): string {
  if (!UUID_RE.test(relocationId)) throw new Error("invalid Tailscale relocation identity");
  return win32.join(
    serviceDir,
    "data",
    "updater",
    "coordinator-relocation",
    relocationId,
    "tailscale-before.json",
  );
}

async function captureTailscale(serviceDir: string, relocationId: string): Promise<string> {
  const path = tailscaleRollbackPath(serviceDir, relocationId);
  await runTrustedTailscale(["serve", "get-config", path, "--all"]);
  await windowsProtectUpdaterArtifact(path, "private");
  return new TextDecoder().decode(
    await windowsReadUpdaterArtifact(path, "private", 1024 * 1024),
  );
}

async function applyTailscale(): Promise<void> {
  if (process.env.ROOST_FRONTED === "0") return;
  await runTrustedTailscale([
    "serve",
    "--bg",
    "--https=4102",
    "http://127.0.0.1:4103",
  ]);
}

async function restoreTailscale(
  serviceDir: string,
  relocationId: string,
  prior: string,
): Promise<void> {
  if (!prior.trim() || prior.trim() === "{}" || prior.trim() === "null") {
    await runTrustedTailscale(["serve", "reset"]);
    return;
  }
  await runTrustedTailscale([
    "serve",
    "set-config",
    tailscaleRollbackPath(serviceDir, relocationId),
    "--all",
  ]);
}

async function coordinatorHealthy(targetUrl: string): Promise<boolean> {
  const response = await fetch(
    `${targetUrl.replace(/\/$/, "")}/roost.v1.CoordinatorService/MiscHealth`,
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{}",
      signal: AbortSignal.timeout(5_000),
    },
  ).catch(() => null);
  return response?.ok === true;
}
