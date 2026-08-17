import { durableWriteFile } from "@roost/shared/durability";
import {
  COORD_LABEL_DARWIN,
  COORD_LABEL_LINUX,
  WORKER_LABEL_DARWIN,
  WORKER_LABEL_LINUX,
  roostServiceDir,
} from "@roost/shared/paths";
import {
  resolveWindowsHelperPath,
  runWindowsHelper,
  windowsApplyServiceDacl,
  windowsConfigureServiceSid,
  windowsResolveServiceSid,
  windowsRevokeServiceDacl,
  windowsReplaceUpdaterArtifact,
  type WindowsServiceControlGrant,
  windowsConfigureService,
  type WindowsServiceConfig,
  type WindowsServiceRecoveryPolicy,
  type WindowsServiceSnapshot as NativeWindowsServiceSnapshot,
  type WindowsServiceSidType,
} from "@roost/shared/windows-helper";
import { dirname, join, win32 } from "node:path";

/** The command-string helpers below are retained for POSIX deploy callers. */
export type PosixServiceOs = "darwin" | "linux";
export type ServiceOs = PosixServiceOs | "win32";

export const WORKER_UNIT = `${WORKER_LABEL_LINUX}.service`;
export const WORKER_AGENT = WORKER_LABEL_DARWIN;
export const COORD_UNIT = `${COORD_LABEL_LINUX}.service`;
export const COORD_AGENT = COORD_LABEL_DARWIN;

// systemd --user over ssh has no login session, so XDG_RUNTIME_DIR is
// unset and systemctl can't find the user bus. Keep this byte-for-byte stable:
// these strings are handed to bash locally and over ssh by POSIX-only callers.
const XDG = `export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/$(id -u)}";`;

function posixServiceCommand(
  os: ServiceOs,
  linux: () => string,
  darwin: () => string,
): string {
  switch (os) {
    case "linux":
      return linux();
    case "darwin":
      return darwin();
    case "win32":
      throw new Error("Windows services require createWindowsServiceManager(); POSIX command strings are disabled");
    default:
      return assertNever(os);
  }
}

export function restartWorkerCmd(os: ServiceOs): string {
  return posixServiceCommand(
    os,
    () => `${XDG} systemctl --user restart ${WORKER_UNIT}`,
    () => `launchctl kickstart -k gui/$(id -u)/${WORKER_AGENT}`,
  );
}

export function verifyWorkerCmd(os: ServiceOs): string {
  return posixServiceCommand(
    os,
    () => `${XDG} systemctl --user show ${WORKER_UNIT} -p ActiveState -p SubState -p MainPID`,
    () => `set -o pipefail; launchctl print gui/$(id -u)/${WORKER_AGENT} 2>&1 | grep -E '^\\s*(state|pid|active count)' | head -5`,
  );
}

export function restartCoordCmd(os: ServiceOs): string {
  return posixServiceCommand(
    os,
    () => `${XDG} systemctl --user daemon-reload && systemctl --user restart ${COORD_UNIT}`,
    () => `launchctl kickstart -k gui/$(id -u)/${COORD_AGENT}`,
  );
}

export function verifyCoordCmd(os: ServiceOs): string {
  return posixServiceCommand(
    os,
    () => `${XDG} systemctl --user is-active ${COORD_UNIT}`,
    () => `launchctl print gui/$(id -u)/${COORD_AGENT} 2>&1 | grep -E '^\\s*(state|pid|active count)' | head -5`,
  );
}

export function stopServicesCmd(os: ServiceOs): string {
  return posixServiceCommand(
    os,
    () => `${XDG} systemctl --user stop ${COORD_UNIT} ${WORKER_UNIT}`,
    () => `launchctl bootout gui/$(id -u)/${COORD_AGENT} 2>/dev/null; launchctl bootout gui/$(id -u)/${WORKER_AGENT} 2>/dev/null; true`,
  );
}

export function currentServiceOs(): ServiceOs {
  switch (process.platform) {
    case "darwin":
    case "linux":
    case "win32":
      return process.platform;
    default:
      throw new Error(`unsupported service platform: ${process.platform}`);
  }
}

export type RoostServiceRole = "keeper" | "worker" | "coordinator" | "updater";

export const WINDOWS_SERVICE_ROLES = [
  "keeper",
  "worker",
  "coordinator",
  "updater",
] as const satisfies readonly RoostServiceRole[];

export const WINDOWS_SERVICE_NAMES = Object.freeze({
  keeper: "RoostKeeperV2",
  worker: "RoostWorkerV2",
  coordinator: "RoostCoordinatorV2",
  updater: "RoostUpdaterV2",
} as const satisfies Readonly<Record<RoostServiceRole, string>>);

export type WindowsServiceState =
  | "missing"
  | "stopped"
  | "start-pending"
  | "stop-pending"
  | "running"
  | "continue-pending"
  | "pause-pending"
  | "paused"
  | "unknown";

export type WindowsServiceStartMode = "automatic" | "manual" | "disabled" | "unknown";

export interface WindowsServiceSnapshot {
  role: RoostServiceRole;
  name: (typeof WINDOWS_SERVICE_NAMES)[RoostServiceRole];
  installed: boolean;
  state: WindowsServiceState;
  startMode: WindowsServiceStartMode;
  imagePath: string | null;
  account: string | null;
  dependencies: readonly string[];
  /** Exact SCM Environment registry values; null means the service is absent. */
  environment?: Readonly<Record<string, string>> | null;
  displayName: string | null;
  description: string | null;
  recoveryPolicy: WindowsServiceRecoveryPolicy | null;
  /** Present only when queried with includeSecurity; null means the service is absent. */
  securityDescriptor?: string | null;
  /** Returned by both basic and full native service queries; null means the service is absent. */
  serviceSidType?: WindowsServiceSidType | null;
}

export type WindowsServiceSnapshotSet = Readonly<Record<RoostServiceRole, WindowsServiceSnapshot>>;
export interface WindowsServiceQueryOptions {
  /** Include the full SCM security descriptor. This requires READ_CONTROL. */
  includeSecurity?: boolean;
}


export interface WindowsServiceDefinition {
  role: RoostServiceRole;
  name: (typeof WINDOWS_SERVICE_NAMES)[RoostServiceRole];
  displayName: string;
  description: string;
  startMode: Extract<WindowsServiceStartMode, "automatic" | "manual">;
  account: string;
  dependencies: readonly RoostServiceRole[];
  shawlPath: string;
  shawlArguments: readonly string[];
  /** Admin-owned stable helper which resolves and verifies the active release. */
  serviceLauncherPath: string;
  executablePath: string;
  arguments: readonly string[];
  cwd: string;
  logDir: string;
  environment: Readonly<Record<string, string>>;
  imagePath: string;
}

export interface WindowsServiceCredentials {
  account: string;
  /** Required for ordinary users on first install/account change. Omit for a gMSA. */
  password?: string;
}

export interface WindowsServiceRestoreOptions {
  /**
   * Configuration is always restored. Lifecycle restoration defaults to the
   * worker and coordinator only so update rollback can never stop the keeper.
   * Pass [] for a configuration-only restore.
   */
  restoreLifecycleRoles?: readonly RoostServiceRole[];
  /** Elevated install rollback may stop/delete a keeper created by that transaction. */
  allowKeeperStop?: boolean;
}

export interface WindowsServiceManager {
  query(
    role: RoostServiceRole,
    options?: WindowsServiceQueryOptions,
  ): Promise<WindowsServiceSnapshot>;
  snapshot(options?: WindowsServiceQueryOptions): Promise<WindowsServiceSnapshotSet>;
  install(
    definition: WindowsServiceDefinition,
    credentials?: WindowsServiceCredentials,
  ): Promise<WindowsServiceSnapshot>;
  configure(
    definition: WindowsServiceDefinition,
    credentials?: WindowsServiceCredentials,
  ): Promise<WindowsServiceSnapshot>;
  start(role: RoostServiceRole): Promise<WindowsServiceSnapshot>;
  stop(role: RoostServiceRole, options?: { timeoutMs?: number }): Promise<WindowsServiceSnapshot>;
  restore(
    snapshot: WindowsServiceSnapshotSet,
    options?: WindowsServiceRestoreOptions,
  ): Promise<WindowsServiceSnapshotSet>;
  provisionServiceLogon(account: string): Promise<void>;
  /** Elevated-install only: configure service SIDs and exact cross-service control grants. */
  provisionServiceSecurity(interactiveSid: string): Promise<void>;
}

export interface WindowsServiceDefinitionOptions {
  executablePath: string;
  shawlPath: string;
  serviceLauncherPath: string;
  windowsHelperPath: string;
  account: string;
  coordinatorHost: boolean;
  serviceDir?: string;
  commonEnvironment?: Readonly<Record<string, string>>;
  roleEnvironment?: Partial<Readonly<Record<RoostServiceRole, Readonly<Record<string, string>>>>>;
  keeperArguments?: readonly string[];
  updaterArguments?: readonly string[];
}

export interface WindowsNativeCommandResult {
  exitCode: number;
  stdout: string;
  stderr: string;
}

export type WindowsNativeCommandRunner = (
  argv: readonly string[],
) => Promise<WindowsNativeCommandResult>;

export interface WindowsServiceManagerOptions {
  platform?: NodeJS.Platform;
  /** Test seam; production receives `[Environment]::SystemDirectory` from the signed installer. */
  systemDirectory?: string;
  run?: WindowsNativeCommandRunner;
  sleep?: (milliseconds: number) => Promise<void>;
  now?: () => number;
  configureNative?: (
    service: (typeof WINDOWS_SERVICE_NAMES)[RoostServiceRole],
    config: WindowsServiceConfig,
  ) => Promise<unknown>;
  configureServiceSidNative?: (
    service: (typeof WINDOWS_SERVICE_NAMES)[RoostServiceRole],
    serviceSidType: WindowsServiceSidType,
  ) => Promise<unknown>;
}

function assertNever(value: never): never {
  throw new Error(`unhandled service platform: ${String(value)}`);
}


const START_MODE_SC_VALUE: Readonly<Record<"automatic" | "manual" | "disabled", string>> = {
  automatic: "auto",
  manual: "demand",
  disabled: "disabled",
};

const DISPLAY_NAMES: Readonly<Record<RoostServiceRole, string>> = {
  keeper: "Roost Keeper V2",
  worker: "Roost Worker V2",
  coordinator: "Roost Coordinator V2",
  updater: "Roost Updater V2",
};

const DESCRIPTIONS: Readonly<Record<RoostServiceRole, string>> = {
  keeper: "Owns persistent Roost terminal processes for the local operator.",
  worker: "Connects this Windows host to the Roost coordinator.",
  coordinator: "Hosts the Roost coordinator and web application.",
  updater: "Applies a verified Roost release transaction on demand.",
};

const EXPECTED_RECOVERY_POLICY: WindowsServiceRecoveryPolicy = {
  resetPeriodSeconds: 86_400,
  rebootMessage: "",
  command: "",
  actions: [
    { type: "restart", delayMs: 5_000 },
    { type: "restart", delayMs: 30_000 },
    { type: "restart", delayMs: 60_000 },
  ],
  actionsOnNonCrashFailures: true,
};

const ALLOWED_SC_MUTATIONS = new Set([
  "create",
  "config",
  "description",
  "failure",
  "failureflag",
  "start",
  "stop",
  "delete",
]);

function assertRole(value: string): asserts value is RoostServiceRole {
  if (!(WINDOWS_SERVICE_ROLES as readonly string[]).includes(value)) {
    throw new Error(`Windows service role is not allowlisted: ${value}`);
  }
}

function serviceName(role: RoostServiceRole): (typeof WINDOWS_SERVICE_NAMES)[RoostServiceRole] {
  assertRole(role);
  return WINDOWS_SERVICE_NAMES[role];
}

function assertNoCommandControl(value: string, label: string): void {
  if (!value || /[\0\r\n]/.test(value)) {
    throw new Error(`${label} must be non-empty and contain no NUL or newline`);
  }
}

function normalizeAccount(account: string): string {
  return account.trim().replace(/^\.\//, ".\\").toLocaleLowerCase("en-US");
}

export function assertWindowsOperatorAccount(account: string): void {
  assertNoCommandControl(account, "Windows service account");
  const normalized = normalizeAccount(account);
  if (
    normalized === "localsystem"
    || normalized === "system"
    || normalized === ".\\localsystem"
    || normalized === "nt authority\\system"
  ) {
    throw new Error("Roost Windows services require an explicit operator account; LocalSystem is forbidden");
  }
}

/** Quote one argv element using the CommandLineToArgvW/CreateProcess rules. */
export function quoteWindowsArg(argument: string): string {
  if (argument.length > 0 && !/[\s"]/.test(argument)) return argument;
  let quoted = "\"";
  let slashes = 0;
  for (const character of argument) {
    if (character === "\\") {
      slashes += 1;
      continue;
    }
    if (character === "\"") {
      quoted += "\\".repeat(slashes * 2 + 1);
      quoted += "\"";
      slashes = 0;
      continue;
    }
    quoted += "\\".repeat(slashes);
    quoted += character;
    slashes = 0;
  }
  quoted += "\\".repeat(slashes * 2);
  quoted += "\"";
  return quoted;
}

function createImagePath(executable: string, argv: readonly string[]): string {
  assertNoCommandControl(executable, "service executable path");
  for (const argument of argv) {
    if (/[\0\r\n]/.test(argument)) throw new Error("service argv contains a NUL or newline");
  }
  return [executable, ...argv].map(quoteWindowsArg).join(" ");
}

function checkedEnvironment(
  environment: Readonly<Record<string, string>>,
): Readonly<Record<string, string>> {
  const checked: Record<string, string> = {};
  for (const [key, value] of Object.entries(environment)) {
    if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(key)) {
      throw new Error(`invalid Windows service environment key: ${key}`);
    }
    if (/[\0\r\n]/.test(value)) {
      throw new Error(`Windows service environment ${key} contains a NUL or newline`);
    }
    checked[key] = value;
  }
  return Object.freeze(checked);
}

function makeWindowsDefinition(
  role: RoostServiceRole,
  options: {
    executablePath: string;
    shawlPath: string;
    serviceLauncherPath: string;
    account: string;
    startMode: "automatic" | "manual";
    dependencies: readonly RoostServiceRole[];
    arguments: readonly string[];
    cwd: string;
    logDir: string;
    environment: Readonly<Record<string, string>>;
    launchCurrent?: boolean;
  },
): WindowsServiceDefinition {
  const name = serviceName(role);
  const environment = checkedEnvironment(options.environment);
  const targetArguments = options.launchCurrent === false
    ? [options.executablePath, ...options.arguments]
    : [options.serviceLauncherPath, "launch-current", ...options.arguments];
  const shawlArguments = [
    "run",
    "--name",
    name,
    "--no-restart",
    "--kill-process-tree",
    "--stop-timeout",
    "15000",
    "--cwd",
    options.cwd,
    "--log-dir",
    options.logDir,
    "--log-as",
    role,
    "--log-cmd-as",
    `${role}-stdio`,
    "--log-rotate",
    "bytes=2097152",
    "--log-retain",
    "2",
    ...Object.entries(environment).flatMap(([key, value]) => ["--env", `${key}=${value}`]),
    "--",
    ...targetArguments,
  ] as const;
  return Object.freeze({
    role,
    name,
    displayName: DISPLAY_NAMES[role],
    description: DESCRIPTIONS[role],
    startMode: options.startMode,
    account: options.account,
    dependencies: Object.freeze([...options.dependencies]),
    shawlPath: options.shawlPath,
    shawlArguments: Object.freeze([...shawlArguments]),
    serviceLauncherPath: options.serviceLauncherPath,
    executablePath: options.executablePath,
    arguments: Object.freeze([...options.arguments]),
    cwd: options.cwd,
    logDir: options.logDir,
    environment,
    imagePath: createImagePath(options.shawlPath, shawlArguments),
  });
}

/**
 * Generate the complete four-service topology. Worker-only installs still
 * create the coordinator service, but leave it demand-start and stopped.
 */
export function buildWindowsServiceDefinitions(
  options: WindowsServiceDefinitionOptions,
): Readonly<Record<RoostServiceRole, WindowsServiceDefinition>> {
  assertWindowsOperatorAccount(options.account);
  assertNoCommandControl(options.executablePath, "Roost executable path");
  assertNoCommandControl(options.shawlPath, "Shawl executable path");
  assertNoCommandControl(options.serviceLauncherPath, "stable Windows service launcher path");
  assertNoCommandControl(options.windowsHelperPath, "Windows helper path");

  const serviceDir = options.serviceDir ?? roostServiceDir(undefined, "win32");
  // Every Windows service gets machine-owned state beneath the protected
  // service root. Never derive daemon state from the installing user's profile.
  const keeperData = join(serviceDir, "data", "keeper");
  const keeperLogs = join(serviceDir, "logs", "keeper");
  const workerData = join(serviceDir, "data", "worker");
  const workerLogs = join(serviceDir, "logs", "worker");
  const coordinatorData = join(serviceDir, "data", "coordinator");
  const coordinatorLogs = join(serviceDir, "logs", "coordinator");
  const updaterData = join(serviceDir, "data", "updater");
  const updaterLogs = join(serviceDir, "logs", "updater");
  const common = options.commonEnvironment ?? {};
  const roleEnv = (role: RoostServiceRole, required: Record<string, string>) => ({
    ...common,
    ...(options.roleEnvironment?.[role] ?? {}),
    ...required,
    ROOST_LOG_ENCODING: "utf-8",
    ROOST_SERVICE_ROLE: role,
  });
  const definitions: Record<RoostServiceRole, WindowsServiceDefinition> = {
    keeper: makeWindowsDefinition("keeper", {
      executablePath: options.executablePath,
      shawlPath: options.shawlPath,
      serviceLauncherPath: options.serviceLauncherPath,
      account: options.account,
      startMode: "automatic",
      dependencies: [],
      arguments: options.keeperArguments ?? ["keeper", "--service"],
      cwd: keeperData,
      logDir: join(keeperLogs, "keeper"),
      environment: roleEnv("keeper", {
        // Keeper still consumes the legacy Worker-named variables, but its
        // identity and logs are isolated from the Worker service roots.
        ROOST_WORKER_DATA_DIR: keeperData,
        ROOST_WORKER_LOG_DIR: keeperLogs,
      }),
    }),
    worker: makeWindowsDefinition("worker", {
      executablePath: options.executablePath,
      shawlPath: options.shawlPath,
      serviceLauncherPath: options.serviceLauncherPath,
      account: options.account,
      startMode: "automatic",
      dependencies: ["keeper"],
      arguments: ["worker"],
      cwd: workerData,
      logDir: join(workerLogs, "worker"),
      environment: roleEnv("worker", {
        ROOST_WORKER_DATA_DIR: workerData,
        ROOST_WORKER_LOG_DIR: workerLogs,
      }),
    }),
    coordinator: makeWindowsDefinition("coordinator", {
      executablePath: options.executablePath,
      shawlPath: options.shawlPath,
      serviceLauncherPath: options.serviceLauncherPath,
      account: options.account,
      startMode: options.coordinatorHost ? "automatic" : "manual",
      dependencies: [],
      arguments: ["coord"],
      cwd: coordinatorData,
      logDir: join(coordinatorLogs, "coordinator"),
      environment: roleEnv("coordinator", {
        ROOST_COORD_DATA_DIR: coordinatorData,
        ROOST_COORD_LOG_DIR: coordinatorLogs,
      }),
    }),
    updater: makeWindowsDefinition("updater", {
      executablePath: options.executablePath,
      // The updater must be able to replace the stable launchers used by the
      // other three stopped roles. Run its one-shot broker from the immutable
      // version directory instead of mapping either stable binary itself.
      shawlPath: join(dirname(options.executablePath), "shawl.exe"),
      account: options.account,
      serviceLauncherPath: options.windowsHelperPath,
      launchCurrent: false,
      // Starts once per boot so an interrupted transaction resumes even when
      // the worker/coordinator binaries or service definitions are unusable.
      // With no pending journal the broker exits successfully immediately.
      startMode: "automatic",
      dependencies: [],
      arguments: options.updaterArguments ?? ["__windows-updater-broker"],
      cwd: updaterData,
      logDir: updaterLogs,
      environment: roleEnv("updater", {
        ROOST_SERVICE_DIR: serviceDir,
      }),
    }),
  };
  return Object.freeze(definitions);
}
export function retargetWindowsUpdaterDefinition(
  current: WindowsServiceDefinition,
  versionDir: string,
): WindowsServiceDefinition {
  if (current.role !== "updater") {
    throw new Error("only the Windows updater service has an immutable version target");
  }
  return makeWindowsDefinition("updater", {
    executablePath: join(versionDir, "roost.exe"),
    shawlPath: join(versionDir, "shawl.exe"),
    serviceLauncherPath: join(versionDir, "roost-win-helper.exe"),
    account: current.account,
    startMode: current.startMode,
    dependencies: current.dependencies,
    arguments: current.arguments,
    cwd: current.cwd,
    logDir: current.logDir,
    environment: current.environment,
    launchCurrent: false,
  });
}


export function windowsServiceDefinitionsPath(
  env: Record<string, string | undefined> = process.env,
): string {
  return join(env.ROOST_SERVICE_DIR ?? roostServiceDir(undefined, "win32"), "service-definitions.json");
}

export async function storeWindowsServiceDefinitions(
  definitions: Readonly<Record<RoostServiceRole, WindowsServiceDefinition>>,
): Promise<void> {
  for (const role of WINDOWS_SERVICE_ROLES) {
    if (!definitions[role]) throw new Error(`missing Windows service definition: ${role}`);
    validateDefinition(definitions[role]);
  }
  const path = windowsServiceDefinitionsPath();
  const contents = `${JSON.stringify({ schemaVersion: 2, services: definitions }, null, 2)}\n`;
  if (process.platform === "win32") {
    await windowsReplaceUpdaterArtifact(path, "control", Buffer.from(contents, "utf8"));
    return;
  }
  await durableWriteFile(path, contents, {
    mode: 0o600,
    privateDacl: true,
  });
}

export async function loadWindowsServiceDefinitions(): Promise<
  Readonly<Record<RoostServiceRole, WindowsServiceDefinition>>
> {
  const raw = await Bun.file(windowsServiceDefinitionsPath()).text();
  const parsed = JSON.parse(raw) as {
    schemaVersion?: unknown;
    services?: Partial<Record<RoostServiceRole, WindowsServiceDefinition>>;
  };
  if (parsed.schemaVersion !== 2 || !parsed.services) {
    throw new Error(
      "legacy Windows service topology requires migration by the signed elevated Roost installer",
    );
  }
  const definitions = parsed.services;
  const keys = Object.keys(definitions).sort();
  if (
    keys.length !== WINDOWS_SERVICE_ROLES.length
    || !WINDOWS_SERVICE_ROLES.every((role) => keys.includes(role))
  ) {
    throw new Error("Windows service-definitions.json must contain exactly four allowlisted roles");
  }
  for (const role of WINDOWS_SERVICE_ROLES) validateDefinition(definitions[role]!);
  return Object.freeze(definitions) as Readonly<Record<RoostServiceRole, WindowsServiceDefinition>>;
}

async function defaultWindowsRunner(
  argv: readonly string[],
): Promise<WindowsNativeCommandResult> {
  const child = Bun.spawn({
    cmd: [...argv],
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
  });
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ]);
  return { exitCode, stdout, stderr };
}

function missingSnapshot(
  role: RoostServiceRole,
  includeSecurity: boolean,
): WindowsServiceSnapshot {
  return {
    role,
    name: serviceName(role),
    installed: false,
    state: "missing",
    startMode: "unknown",
    imagePath: null,
    account: null,
    dependencies: [],
    displayName: null,
    description: null,
    recoveryPolicy: null,
    environment: null,
    serviceSidType: null,
    ...(includeSecurity ? { securityDescriptor: null } : {}),
  };
}


function isMissingService(result: WindowsNativeCommandResult): boolean {
  const text = `${result.stdout}\n${result.stderr}`;
  return result.exitCode !== 0
    && (/\b1060\b/.test(text) || /specified service does not exist/i.test(text));
}

function commandFailure(argv: readonly string[], result: WindowsNativeCommandResult): Error {
  const detail = result.stderr.trim() || result.stdout.trim() || `exit ${result.exitCode}`;
  return new Error(`${argv[0]} ${argv[1] ?? ""} failed: ${detail}`);
}

function sameAccount(left: string | null, right: string): boolean {
  return left !== null && normalizeAccount(left) === normalizeAccount(right);
}

function sameSet(left: readonly string[], right: readonly string[]): boolean {
  const a = [...left].map((value) => value.toLocaleLowerCase("en-US")).sort();
  const b = [...right].map((value) => value.toLocaleLowerCase("en-US")).sort();
  return a.length === b.length && a.every((value, index) => value === b[index]);
}

function sameEnvironment(
  left: Readonly<Record<string, string>> | null | undefined,
  right: Readonly<Record<string, string>> | null | undefined,
): boolean {
  if (left === null || left === undefined || right === null || right === undefined) {
    return left === right;
  }
  const byKey = ([leftKey]: readonly [string, string], [rightKey]: readonly [string, string]) =>
    leftKey < rightKey ? -1 : leftKey > rightKey ? 1 : 0;
  const leftEntries = Object.entries(left).sort(byKey);
  const rightEntries = Object.entries(right).sort(byKey);
  return leftEntries.length === rightEntries.length
    && leftEntries.every(([key, value], index) =>
      key === rightEntries[index]?.[0] && value === rightEntries[index]?.[1]);
}

function isWindowsServiceSidType(value: unknown): value is WindowsServiceSidType {
  return value === "none" || value === "restricted" || value === "unrestricted";
}

function sameRecoveryPolicy(
  left: WindowsServiceRecoveryPolicy | null,
  right: WindowsServiceRecoveryPolicy,
): boolean {
  return left !== null
    && left.resetPeriodSeconds === right.resetPeriodSeconds
    && left.rebootMessage === right.rebootMessage
    && left.command === right.command
    && left.actionsOnNonCrashFailures === right.actionsOnNonCrashFailures
    && left.actions.length === right.actions.length
    && left.actions.every((action, index) =>
      action.type === right.actions[index]?.type
      && action.delayMs === right.actions[index]?.delayMs);
}

function validateDefinition(definition: WindowsServiceDefinition): void {
  assertRole(definition.role);
  if (definition.name !== serviceName(definition.role)) {
    throw new Error(`service name for ${definition.role} must be ${serviceName(definition.role)}`);
  }
  assertWindowsOperatorAccount(definition.account);
  const expectedImagePath = createImagePath(definition.shawlPath, definition.shawlArguments);
  if (definition.imagePath !== expectedImagePath) {
    throw new Error(`service imagePath for ${definition.role} does not match its structured Shawl argv`);
  }
  const separator = definition.shawlArguments.indexOf("--");
  const child = separator < 0 ? [] : definition.shawlArguments.slice(separator + 1);
  if (
    separator < 1
    || child[0] !== definition.serviceLauncherPath
    || child[1] !== "launch-current"
    || child.length !== definition.arguments.length + 2
    || child.slice(2).some((argument, index) => argument !== definition.arguments[index])
  ) {
    throw new Error(`${definition.name} must launch through the stable signed helper`);
  }
  if (Object.keys(definition.environment).some((key) => key.toUpperCase() === "ROOST_WIN_HELPER")) {
    throw new Error(`${definition.name} must let the stable launcher select ROOST_WIN_HELPER`);
  }
  for (const dependency of definition.dependencies) assertRole(dependency);
  if (definition.role === "worker") {
    if (!sameSet(definition.dependencies.map(serviceName), [WINDOWS_SERVICE_NAMES.keeper])) {
      throw new Error("RoostWorkerV2 must depend on RoostKeeperV2 and no other service");
    }
  } else if (definition.dependencies.length !== 0) {
    throw new Error(`${definition.name} must remain independent`);
  }
  if (definition.role === "updater" && definition.startMode !== "automatic") {
    throw new Error("RoostUpdaterV2 must start once per boot for transaction recovery");
  }
}

function credentialPassword(
  definition: WindowsServiceDefinition,
  credentials: WindowsServiceCredentials | undefined,
  required: boolean,
): string | undefined {
  if (credentials && normalizeAccount(credentials.account) !== normalizeAccount(definition.account)) {
    throw new Error(`credentials account does not match ${definition.name} service account`);
  }
  const password = credentials?.password;
  const gmsa = definition.account.endsWith("$");
  if (required && password === undefined && !gmsa) {
    throw new Error(`password is required to assign ${definition.name} to ${definition.account}`);
  }
  if (password !== undefined && /[\0\r\n]/.test(password)) {
    throw new Error("Windows service password contains a NUL or newline");
  }
  return password;
}

function definitionConfigArgs(definition: WindowsServiceDefinition): string[] {
  const dependencies = definition.dependencies.map(serviceName);
  return [
    "binPath=",
    definition.imagePath,
    "start=",
    START_MODE_SC_VALUE[definition.startMode],
    "depend=",
    dependencies.length > 0 ? dependencies.join("/") : "/",
    "DisplayName=",
    definition.displayName,
  ];
}

function assertDefinitionApplied(
  definition: WindowsServiceDefinition,
  snapshot: WindowsServiceSnapshot,
): void {
  if (!snapshot.installed) throw new Error(`${definition.name} disappeared after SCM mutation`);
  if (snapshot.imagePath !== definition.imagePath) {
    throw new Error(`${definition.name} ImagePath did not round-trip through SCM`);
  }
  if (snapshot.startMode !== definition.startMode) {
    throw new Error(`${definition.name} start mode did not round-trip through SCM`);
  }
  if (!sameAccount(snapshot.account, definition.account)) {
    throw new Error(`${definition.name} operator account did not round-trip through SCM`);
  }
  const expectedDependencies = definition.dependencies.map(serviceName);
  if (!sameSet(snapshot.dependencies, expectedDependencies)) {
    throw new Error(`${definition.name} dependencies did not round-trip through SCM`);
  }
  if (snapshot.displayName !== definition.displayName || snapshot.description !== definition.description) {
    throw new Error(`${definition.name} display metadata did not round-trip through SCM`);
  }
  if (!sameRecoveryPolicy(snapshot.recoveryPolicy, EXPECTED_RECOVERY_POLICY)) {
    throw new Error(`${definition.name} recovery policy did not round-trip through SCM`);
  }
}

export function createWindowsServiceManager(
  options: WindowsServiceManagerOptions = {},
): WindowsServiceManager {
  const platform = options.platform ?? process.platform;
  if (platform !== "win32") {
    throw new Error(`Windows SCM service manager is unavailable on ${platform}`);
  }
  const run = options.run ?? defaultWindowsRunner;
  const sleep = options.sleep ?? Bun.sleep;
  const now = options.now ?? Date.now;
  const systemDirectory = options.systemDirectory ?? process.env.ROOST_SYSTEM32;
  if (!systemDirectory || !win32.isAbsolute(systemDirectory) || /[\0\r\n]/.test(systemDirectory)) {
    throw new Error("trusted absolute ROOST_SYSTEM32 is required for Windows SCM operations");
  }
  const scPath = win32.join(win32.resolve(systemDirectory), "sc.exe");

  const runChecked = async (argv: readonly string[]): Promise<WindowsNativeCommandResult> => {
    const result = await run(argv);
    if (result.exitCode !== 0) throw commandFailure(argv, result);
    return result;
  };
  const configureNative = options.configureNative
    ?? ((service: (typeof WINDOWS_SERVICE_NAMES)[RoostServiceRole], config: WindowsServiceConfig) =>
      windowsConfigureService(service, config));
  const configureServiceSidNative = options.configureServiceSidNative
    ?? ((
      service: (typeof WINDOWS_SERVICE_NAMES)[RoostServiceRole],
      serviceSidType: WindowsServiceSidType,
    ) => windowsConfigureServiceSid(service, serviceSidType));

  const query = async (
    role: RoostServiceRole,
    queryOptions: WindowsServiceQueryOptions = {},
  ): Promise<WindowsServiceSnapshot> => {
    const name = serviceName(role);
    const includeSecurity = queryOptions.includeSecurity === true;
    const argv = includeSecurity
      ? [resolveWindowsHelperPath(), "service-query", name]
      : [resolveWindowsHelperPath(), "service-query", name, "basic"];
    const result = await run(argv);
    if (isMissingService(result)) return missingSnapshot(role, includeSecurity);
    if (result.exitCode !== 0) throw commandFailure(argv, result);
    let native: NativeWindowsServiceSnapshot;
    try {
      native = JSON.parse(result.stdout) as NativeWindowsServiceSnapshot;
    } catch (error) {
      throw new Error(`${name} structured service query returned invalid JSON: ${String(error)}`);
    }
    if (native.name !== name) throw new Error(`${name} structured service query returned ${native.name}`);
    if (!isWindowsServiceSidType(native.serviceSidType)) {
      throw new Error(`${name} structured service query returned an invalid service SID type`);
    }
    if (
      includeSecurity
      && (typeof native.securityDescriptor !== "string" || native.securityDescriptor.trim() === "")
    ) {
      throw new Error(`${name} full service query omitted its security descriptor`);
    }
    if (
      native.environment === null
      || typeof native.environment !== "object"
      || Object.values(native.environment).some((value) => typeof value !== "string")
    ) {
      throw new Error(`${name} structured service query returned an invalid environment`);
    }
    return {
      role,
      name,
      installed: true,
      state: native.state,
      startMode: native.startType,
      imagePath: native.imagePathRaw,
      account: native.account,
      dependencies: Object.freeze([...native.dependencies]),
      environment: Object.freeze({ ...native.environment }),
      displayName: native.displayName,
      description: native.description,
      recoveryPolicy: {
        ...native.recoveryPolicy,
        actions: native.recoveryPolicy.actions.map((action) => ({ ...action })),
      },
      serviceSidType: native.serviceSidType,
      ...(includeSecurity ? { securityDescriptor: native.securityDescriptor } : {}),
    };
  };

  const snapshot = async (
    queryOptions: WindowsServiceQueryOptions = {},
  ): Promise<WindowsServiceSnapshotSet> => {
    const entries = await Promise.all(
      WINDOWS_SERVICE_ROLES.map(async (role) => [role, await query(role, queryOptions)] as const),
    );
    return Object.freeze(Object.fromEntries(entries)) as WindowsServiceSnapshotSet;
  };

  /**
   * Every mutation goes through this gate. It performs an SCM round-trip both
   * immediately before and immediately after the allowlisted operation.
   */
  const mutate = async (
    role: RoostServiceRole,
    argv: readonly string[],
  ): Promise<WindowsServiceSnapshot> => {
    const name = serviceName(role);
    if (
      argv[0] !== scPath
      || !ALLOWED_SC_MUTATIONS.has(argv[1] ?? "")
      || argv[2] !== name
    ) {
      throw new Error(`refusing non-allowlisted SCM mutation for ${role}`);
    }
    await query(role);
    await runChecked(argv);
    return query(role);
  };

  const configureRecovery = async (role: RoostServiceRole): Promise<void> => {
    const name = serviceName(role);
    await mutate(role, [
      scPath,
      "failure",
      name,
      "reset=",
      "86400",
      "actions=",
      "restart/5000/restart/30000/restart/60000",
    ]);
    // Recovery must also apply to clean Shawl exits that report a service error.
    await mutate(role, [scPath, "failureflag", name, "1"]);
  };

  const resolveServiceAccount = async (
    account: string,
  ): Promise<{ sid: string; canonicalAccount: string; localAccount: boolean; administrator: boolean }> => {
    assertWindowsOperatorAccount(account);
    const resolved = await runWindowsHelper<{
      sid: string;
      canonicalAccount: string;
      localAccount: boolean;
      administrator: boolean;
    }>("resolve-account-sid", [account]);
    if (!/^S-\d(?:-\d+)+$/.test(resolved.sid)) {
      throw new Error("Windows helper returned an invalid operator SID");
    }
    assertWindowsOperatorAccount(resolved.canonicalAccount);
    if (typeof resolved.localAccount !== "boolean" || typeof resolved.administrator !== "boolean") {
      throw new Error("Windows helper omitted service-account privilege metadata");
    }
    if (resolved.administrator) {
      throw new Error("Roost Windows services require a non-administrator service account");
    }
    return resolved;
  };

  const provisionAccount = async (
    account: string,
  ): Promise<{ sid: string; canonicalAccount: string }> => {
    const resolved = await resolveServiceAccount(account);
    await runWindowsHelper<{ changed: boolean }>(
      "grant-logon-as-service",
      [resolved.sid],
    );
    return resolved;
  };

  const provisionServiceLogon = async (account: string): Promise<void> => {
    await provisionAccount(account);
  };

  const assertServiceDaclRoundTrip = async (
    role: RoostServiceRole,
    expectedSddl: string,
  ): Promise<void> => {
    const name = serviceName(role);
    const queried = await runWindowsHelper<NativeWindowsServiceSnapshot>("service-query", [name]);
    if (typeof queried.securityDescriptor !== "string") {
      throw new Error(`${name} full service query omitted its security descriptor`);
    }
    const actual = queried.securityDescriptor.replace(/\s+/g, "").toUpperCase();
    const expected = expectedSddl.replace(/\s+/g, "").toUpperCase();
    if (!expected || actual !== expected) {
      throw new Error(`${name} service DACL did not round-trip through SCM`);
    }
  };

  const applyServiceDacl = async (
    role: RoostServiceRole,
    sid: string,
    rights: WindowsServiceControlGrant,
  ): Promise<void> => {
    await query(role);
    const applied = await windowsApplyServiceDacl(serviceName(role), sid, rights);
    await assertServiceDaclRoundTrip(role, applied.sddl);
    await query(role);
  };

  const revokeServiceDacl = async (
    role: RoostServiceRole,
    sid: string,
  ): Promise<void> => {
    await query(role);
    const applied = await windowsRevokeServiceDacl(serviceName(role), sid);
    await assertServiceDaclRoundTrip(role, applied.sddl);
    await query(role);
  };

  const provisionServiceSecurity = async (interactiveSid: string): Promise<void> => {
    if (!/^S-1-(?:\d+-)+\d+$/.test(interactiveSid)) {
      throw new Error("Windows interactive operator SID is invalid");
    }
    const current = await snapshot();
    const firstAccount = current.keeper.account;
    if (
      !firstAccount
      || WINDOWS_SERVICE_ROLES.some((role) => !current[role].installed)
      || WINDOWS_SERVICE_ROLES.some((role) => !sameAccount(current[role].account, firstAccount))
    ) {
      throw new Error("all four Roost services must be installed under one service account before SID hardening");
    }
    const base = await resolveServiceAccount(firstAccount);
    for (const role of WINDOWS_SERVICE_ROLES) {
      await configureServiceSidNative(serviceName(role), "unrestricted");
    }
    const sidEntries = await Promise.all(
      WINDOWS_SERVICE_ROLES.map(async (role) =>
        [role, (await windowsResolveServiceSid(serviceName(role))).sid] as const),
    );
    const serviceSids = Object.fromEntries(sidEntries) as Record<RoostServiceRole, string>;
    // Install every replacement ACE before revoking the shared base identity,
    // so a failed cutover never strands a service without its narrow control
    // principals. Updater gets the exact SCM configuration/lifecycle rights
    // needed for forward activation and rollback; WRITE_DAC/WRITE_OWNER remain
    // forbidden.
    const queryOnly = "QUERY_STATUS,QUERY_CONFIG" as const;
    const startQuery = "START,QUERY_STATUS,QUERY_CONFIG" as const;
    const updaterControl =
      "CHANGE_CONFIG,START,STOP,QUERY_STATUS,QUERY_CONFIG" as const;
    for (const role of WINDOWS_SERVICE_ROLES) {
      await applyServiceDacl(role, serviceSids.updater, updaterControl);
    }
    const lifecycle = "START,STOP,QUERY_STATUS,QUERY_CONFIG" as const;
    await applyServiceDacl("updater", serviceSids.worker, startQuery);
    await applyServiceDacl("updater", serviceSids.coordinator, startQuery);
    await applyServiceDacl("keeper", interactiveSid, lifecycle);
    await applyServiceDacl("updater", interactiveSid, startQuery);
    await applyServiceDacl("worker", interactiveSid, queryOnly);
    await applyServiceDacl("coordinator", interactiveSid, queryOnly);

    for (const role of WINDOWS_SERVICE_ROLES) await revokeServiceDacl(role, base.sid);
  };

  const configureServiceAccount = async (
    definition: WindowsServiceDefinition,
    credentials: WindowsServiceCredentials,
    canonicalAccount: string,
  ): Promise<void> => {
    const password = credentialPassword(definition, credentials, true);
    const input = password === undefined
      ? new Uint8Array()
      : new TextEncoder().encode(password);
    await query(definition.role);
    await runWindowsHelper<{ changed: boolean }>(
      "configure-service-account",
      [definition.name, canonicalAccount],
      { input, sensitive: true },
    );
    const after = await query(definition.role);
    if (!sameAccount(after.account, canonicalAccount)) {
      throw new Error(`${definition.name} operator account did not round-trip through SCM`);
    }
  };

  const configure = async (
    definition: WindowsServiceDefinition,
    credentials?: WindowsServiceCredentials,
  ): Promise<WindowsServiceSnapshot> => {
    validateDefinition(definition);
    const before = await query(definition.role);
    if (!before.installed) {
      throw new Error(`${definition.name} is not installed`);
    }
    const accountChanged = !sameAccount(before.account, definition.account);
    let canonicalAccount = before.account;
    if (accountChanged || credentials !== undefined) {
      if (!credentials) {
        throw new Error(`credential is required to assign ${definition.name} to ${definition.account}`);
      }
      const resolved = await provisionAccount(definition.account);
      canonicalAccount = resolved.canonicalAccount;
      await configureServiceAccount(definition, credentials, canonicalAccount);
    }
    if (!canonicalAccount) throw new Error(`${definition.name} has no configured service account`);
    await mutate(definition.role, [
      scPath,
      "config",
      definition.name,
      ...definitionConfigArgs(definition),
    ]);
    await mutate(definition.role, [
      scPath,
      "description",
      definition.name,
      definition.description,
    ]);
    await configureRecovery(definition.role);
    const after = await query(definition.role);
    assertDefinitionApplied({ ...definition, account: canonicalAccount }, after);
    return after;
  };

  const install = async (
    definition: WindowsServiceDefinition,
    credentials?: WindowsServiceCredentials,
  ): Promise<WindowsServiceSnapshot> => {
    validateDefinition(definition);
    const before = await query(definition.role);
    if (before.installed) {
      if (!sameAccount(before.account, definition.account)) {
        throw new Error(`${definition.name} account migration cannot be rolled back without its prior credential`);
      }
      const configured = await configure(definition, credentials);
      await configureServiceSidNative(definition.name, "unrestricted");
      return configured;
    }
    if (!credentials) {
      throw new Error(`credential is required to install ${definition.name}`);
    }
    credentialPassword(definition, credentials, true);
    await resolveServiceAccount(definition.account);
    const dependencies = definition.dependencies.map(serviceName);
    // New services are inert until the helper has assigned the operator
    // account. No password or other secret ever appears in argv or env.
    await mutate(definition.role, [
      scPath,
      "create",
      definition.name,
      "binPath=",
      definition.imagePath,
      "start=",
      "disabled",
      "depend=",
      dependencies.length > 0 ? dependencies.join("/") : "/",
      "DisplayName=",
      definition.displayName,
    ]);
    const configured = await configure(definition, credentials);
    await configureServiceSidNative(definition.name, "unrestricted");
    return configured;
  };

  const waitForState = async (
    role: RoostServiceRole,
    expected: WindowsServiceState,
    timeoutMs: number,
  ): Promise<WindowsServiceSnapshot> => {
    const deadline = now() + timeoutMs;
    let current = await query(role);
    while (current.state !== expected) {
      if (now() >= deadline) {
        throw new Error(`${current.name} did not reach ${expected} within ${timeoutMs}ms (state ${current.state})`);
      }
      await sleep(250);
      current = await query(role);
    }
    return current;
  };

  const start = async (role: RoostServiceRole): Promise<WindowsServiceSnapshot> => {
    const before = await query(role);
    if (!before.installed) throw new Error(`${serviceName(role)} is not installed`);
    if (before.state === "running") return before;
    await mutate(role, [scPath, "start", serviceName(role)]);
    return waitForState(role, "running", 30_000);
  };

  const stop = async (
    role: RoostServiceRole,
    stopOptions: { timeoutMs?: number } = {},
  ): Promise<WindowsServiceSnapshot> => {
    const before = await query(role);
    if (!before.installed) throw new Error(`${serviceName(role)} is not installed`);
    if (before.state === "stopped") return before;
    if (before.state !== "stop-pending") {
      await mutate(role, [scPath, "stop", serviceName(role)]);
    }
    return waitForState(role, "stopped", stopOptions.timeoutMs ?? 30_000);
  };

  const restoreConfig = async (
    role: RoostServiceRole,
    saved: WindowsServiceSnapshot,
    allowKeeperStop: boolean,
  ): Promise<void> => {
    if (saved.role !== role || saved.name !== serviceName(role)) {
      throw new Error(`invalid saved SCM snapshot for ${role}`);
    }
    const includeSecurity = saved.securityDescriptor !== undefined;
    const current = await query(role);
    if (!saved.installed) {
      if (!current.installed) return;
      if (role === "keeper" && current.state !== "stopped" && !allowKeeperStop) {
        throw new Error("refusing to stop keeper while restoring an absent baseline");
      }
      if (current.state !== "stopped") await stop(role);
      await mutate(role, [scPath, "delete", serviceName(role)]);
      return;
    }
    if (!current.installed) {
      throw new Error(`cannot restore deleted ${saved.name} without its operator credentials`);
    }
    if (
      saved.imagePath === null
      || saved.account === null
      || saved.startMode === "unknown"
      || saved.displayName === null
      || saved.description === null
      || saved.recoveryPolicy === null
    ) {
      throw new Error(`saved ${saved.name} configuration is incomplete`);
    }
    if (
      saved.environment === null
      || (includeSecurity && saved.environment === undefined)
    ) {
      throw new Error(`saved ${saved.name} environment is incomplete`);
    }
    if (
      saved.serviceSidType === null
      || (includeSecurity && saved.serviceSidType === undefined)
    ) {
      throw new Error(`saved ${saved.name} service SID type is incomplete`);
    }
    if (
      saved.serviceSidType !== undefined
      && !isWindowsServiceSidType(saved.serviceSidType)
    ) {
      throw new Error(`saved ${saved.name} service SID type is invalid`);
    }
    if (
      includeSecurity
      && (typeof saved.securityDescriptor !== "string" || saved.securityDescriptor.trim() === "")
    ) {
      throw new Error(`saved ${saved.name} security descriptor is incomplete`);
    }
    if (!sameAccount(current.account, saved.account)) {
      throw new Error(`cannot restore ${saved.name} account without explicit credentials`);
    }
    await mutate(role, [
      scPath,
      "config",
      serviceName(role),
      "binPath=",
      saved.imagePath,
      "start=",
      START_MODE_SC_VALUE[saved.startMode],
      "depend=",
      saved.dependencies.length > 0 ? saved.dependencies.join("/") : "/",
      "DisplayName=",
      saved.displayName,
    ]);
    if (saved.serviceSidType !== undefined && saved.serviceSidType !== null) {
      await configureServiceSidNative(serviceName(role), saved.serviceSidType);
    }
    await configureNative(serviceName(role), {
      description: saved.description,
      recoveryPolicy: saved.recoveryPolicy,
      ...(includeSecurity ? { securityDescriptor: saved.securityDescriptor! } : {}),
      ...(saved.environment !== undefined
        ? { environment: { ...saved.environment! } }
        : {}),
    });
    const after = await query(role, { includeSecurity });
    const securityRestored = !includeSecurity || (
      typeof after.securityDescriptor === "string"
      && after.securityDescriptor.replace(/\s+/g, "").toUpperCase()
        === saved.securityDescriptor!.replace(/\s+/g, "").toUpperCase()
    );
    if (
      after.imagePath !== saved.imagePath
      || after.startMode !== saved.startMode
      || !sameSet(after.dependencies, saved.dependencies)
      || after.displayName !== saved.displayName
      || after.description !== saved.description
      || !sameRecoveryPolicy(after.recoveryPolicy, saved.recoveryPolicy)
      || (
        saved.environment !== undefined
        && !sameEnvironment(after.environment, saved.environment)
      )
      || (
        saved.serviceSidType !== undefined
        && after.serviceSidType !== saved.serviceSidType
      )
      || !securityRestored
    ) {
      throw new Error(`${saved.name} configuration did not restore exactly`);
    }
  };

  const restore = async (
    saved: WindowsServiceSnapshotSet,
    restoreOptions: WindowsServiceRestoreOptions = {},
  ): Promise<WindowsServiceSnapshotSet> => {
    const securitySnapshotCount = WINDOWS_SERVICE_ROLES.filter(
      (role) => saved[role].securityDescriptor !== undefined,
    ).length;
    if (securitySnapshotCount !== 0 && securitySnapshotCount !== WINDOWS_SERVICE_ROLES.length) {
      throw new Error("Windows SCM rollback requires either basic or full snapshots for all four services");
    }
    const includeSecurity = securitySnapshotCount === WINDOWS_SERVICE_ROLES.length;
    for (const role of ["worker", "coordinator", "updater", "keeper"] as const) {
      await restoreConfig(role, saved[role], restoreOptions.allowKeeperStop === true);
    }
    const lifecycleRoles = restoreOptions.restoreLifecycleRoles ?? ["worker", "coordinator"];
    for (const role of lifecycleRoles) {
      const wanted = saved[role];
      if (!wanted.installed) continue;
      const current = await query(role);
      if (wanted.state === "running" && current.state !== "running") {
        await start(role);
      } else if (wanted.state === "stopped" && current.state !== "stopped") {
        if (role === "keeper" && !restoreOptions.allowKeeperStop) {
          throw new Error("refusing to stop keeper during SCM rollback");
        }
        await stop(role);
      }
    }
    return snapshot({ includeSecurity });
  };

  return {
    query,
    snapshot,
    install,
    configure,
    start,
    stop,
    restore,
    provisionServiceLogon,
    provisionServiceSecurity,
  };
}
