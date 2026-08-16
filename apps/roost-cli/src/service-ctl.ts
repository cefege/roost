import { durableWriteFile } from "@roost/shared/durability";
import {
  COORD_LABEL_DARWIN,
  COORD_LABEL_LINUX,
  WORKER_LABEL_DARWIN,
  WORKER_LABEL_LINUX,
  coordDataDir,
  coordLogDir,
  roostServiceDir,
  workerDataDir,
  workerLogDir,
} from "@roost/shared/paths";
import { runWindowsHelper } from "@roost/shared/windows-helper";
import { join } from "node:path";

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
}

export type WindowsServiceSnapshotSet = Readonly<Record<RoostServiceRole, WindowsServiceSnapshot>>;

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
}

export interface WindowsServiceManager {
  query(role: RoostServiceRole): Promise<WindowsServiceSnapshot>;
  snapshot(): Promise<WindowsServiceSnapshotSet>;
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
}

export interface WindowsServiceDefinitionOptions {
  executablePath: string;
  shawlPath: string;
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
  run?: WindowsNativeCommandRunner;
  sleep?: (milliseconds: number) => Promise<void>;
  now?: () => number;
}

function assertNever(value: never): never {
  throw new Error(`unhandled service platform: ${String(value)}`);
}

const WINDOWS_STATE_BY_CODE: Readonly<Record<number, WindowsServiceState>> = {
  1: "stopped",
  2: "start-pending",
  3: "stop-pending",
  4: "running",
  5: "continue-pending",
  6: "pause-pending",
  7: "paused",
};

const WINDOWS_START_MODE_BY_CODE: Readonly<Record<number, WindowsServiceStartMode>> = {
  2: "automatic",
  3: "manual",
  4: "disabled",
};

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
    account: string;
    startMode: "automatic" | "manual";
    dependencies: readonly RoostServiceRole[];
    arguments: readonly string[];
    cwd: string;
    logDir: string;
    environment: Readonly<Record<string, string>>;
  },
): WindowsServiceDefinition {
  const name = serviceName(role);
  const environment = checkedEnvironment(options.environment);
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
    options.executablePath,
    ...options.arguments,
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
  assertNoCommandControl(options.windowsHelperPath, "Windows helper path");

  const serviceDir = options.serviceDir ?? roostServiceDir(undefined, "win32");
  const workerData = workerDataDir();
  const workerLogs = workerLogDir();
  const coordinatorData = coordDataDir();
  const coordinatorLogs = coordLogDir();
  const common = options.commonEnvironment ?? {};
  const roleEnv = (role: RoostServiceRole, required: Record<string, string>) => ({
    ...common,
    ...required,
    ...(options.roleEnvironment?.[role] ?? {}),
    ROOST_LOG_ENCODING: "utf-8",
  });
  const definitions: Record<RoostServiceRole, WindowsServiceDefinition> = {
    keeper: makeWindowsDefinition("keeper", {
      executablePath: options.executablePath,
      shawlPath: options.shawlPath,
      account: options.account,
      startMode: "automatic",
      dependencies: [],
      arguments: options.keeperArguments ?? ["keeper", "--service"],
      cwd: workerData,
      logDir: join(workerLogs, "keeper"),
      environment: roleEnv("keeper", {
        ROOST_WORKER_DATA_DIR: workerData,
        ROOST_WORKER_LOG_DIR: workerLogs,
        ROOST_WIN_HELPER: options.windowsHelperPath,
      }),
    }),
    worker: makeWindowsDefinition("worker", {
      executablePath: options.executablePath,
      shawlPath: options.shawlPath,
      account: options.account,
      startMode: "automatic",
      dependencies: ["keeper"],
      arguments: ["worker"],
      cwd: workerData,
      logDir: join(workerLogs, "worker"),
      environment: roleEnv("worker", {
        ROOST_WORKER_DATA_DIR: workerData,
        ROOST_WORKER_LOG_DIR: workerLogs,
        ROOST_WIN_HELPER: options.windowsHelperPath,
      }),
    }),
    coordinator: makeWindowsDefinition("coordinator", {
      executablePath: options.executablePath,
      shawlPath: options.shawlPath,
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
      shawlPath: options.shawlPath,
      account: options.account,
      startMode: "manual",
      dependencies: [],
      arguments: options.updaterArguments ?? ["__windows-updater-broker"],
      cwd: serviceDir,
      logDir: join(serviceDir, "logs", "updater"),
      environment: roleEnv("updater", {
        ROOST_SERVICE_DIR: serviceDir,
      }),
    }),
  };
  return Object.freeze(definitions);
}

export function windowsServiceDefinitionsPath(): string {
  return join(roostServiceDir(undefined, "win32"), "service-definitions.json");
}

export async function storeWindowsServiceDefinitions(
  definitions: Readonly<Record<RoostServiceRole, WindowsServiceDefinition>>,
): Promise<void> {
  for (const role of WINDOWS_SERVICE_ROLES) {
    if (!definitions[role]) throw new Error(`missing Windows service definition: ${role}`);
    validateDefinition(definitions[role]);
  }
  await durableWriteFile(
    windowsServiceDefinitionsPath(),
    `${JSON.stringify({ schemaVersion: 1, services: definitions }, null, 2)}\n`,
    { platform: "win32", mode: 0o600, privateDacl: true },
  );
}

export async function loadWindowsServiceDefinitions(): Promise<
  Readonly<Record<RoostServiceRole, WindowsServiceDefinition>>
> {
  const raw = await Bun.file(windowsServiceDefinitionsPath()).text();
  const parsed = JSON.parse(raw) as {
    schemaVersion?: unknown;
    services?: Partial<Record<RoostServiceRole, WindowsServiceDefinition>>;
  };
  if (parsed.schemaVersion !== 1 || !parsed.services) {
    throw new Error("unsupported Windows service-definitions.json schema");
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

function missingSnapshot(role: RoostServiceRole): WindowsServiceSnapshot {
  return {
    role,
    name: serviceName(role),
    installed: false,
    state: "missing",
    startMode: "unknown",
    imagePath: null,
    account: null,
    dependencies: [],
  };
}

function scField(output: string, field: string): string | null {
  const escaped = field.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`^\\s*${escaped}\\s*:\\s*(.*)$`, "mi").exec(output)?.[1]?.trim() ?? null;
}

function parseDependencies(output: string): readonly string[] {
  const lines = output.split(/\r?\n/);
  const index = lines.findIndex((line) => /^\s*DEPENDENCIES\s*:/i.test(line));
  if (index < 0) return [];
  const first = lines[index]!.replace(/^\s*DEPENDENCIES\s*:\s*/i, "").trim();
  const dependencies = first ? [first] : [];
  for (let i = index + 1; i < lines.length; i += 1) {
    const continuation = /^\s*:\s*(\S.*)$/.exec(lines[i]!);
    if (!continuation) break;
    dependencies.push(continuation[1]!.trim());
  }
  return Object.freeze(dependencies);
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
  for (const dependency of definition.dependencies) assertRole(dependency);
  if (definition.role === "worker") {
    if (!sameSet(definition.dependencies.map(serviceName), [WINDOWS_SERVICE_NAMES.keeper])) {
      throw new Error("RoostWorkerV2 must depend on RoostKeeperV2 and no other service");
    }
  } else if (definition.dependencies.length !== 0) {
    throw new Error(`${definition.name} must remain independent`);
  }
  if (definition.role === "updater" && definition.startMode !== "manual") {
    throw new Error("RoostUpdaterV2 must be demand-start");
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

  const runChecked = async (argv: readonly string[]): Promise<WindowsNativeCommandResult> => {
    const result = await run(argv);
    if (result.exitCode !== 0) throw commandFailure(argv, result);
    return result;
  };

  const query = async (role: RoostServiceRole): Promise<WindowsServiceSnapshot> => {
    const name = serviceName(role);
    const stateResult = await run(["sc.exe", "query", name]);
    if (isMissingService(stateResult)) return missingSnapshot(role);
    if (stateResult.exitCode !== 0) throw commandFailure(["sc.exe", "query", name], stateResult);
    const configResult = await runChecked(["sc.exe", "qc", name]);
    const stateCode = Number(scField(stateResult.stdout, "STATE")?.match(/^\d+/)?.[0]);
    const startCode = Number(scField(configResult.stdout, "START_TYPE")?.match(/^\d+/)?.[0]);
    return {
      role,
      name,
      installed: true,
      state: WINDOWS_STATE_BY_CODE[stateCode] ?? "unknown",
      startMode: WINDOWS_START_MODE_BY_CODE[startCode] ?? "unknown",
      imagePath: scField(configResult.stdout, "BINARY_PATH_NAME"),
      account: scField(configResult.stdout, "SERVICE_START_NAME"),
      dependencies: parseDependencies(configResult.stdout),
    };
  };

  const snapshot = async (): Promise<WindowsServiceSnapshotSet> => {
    const entries = await Promise.all(
      WINDOWS_SERVICE_ROLES.map(async (role) => [role, await query(role)] as const),
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
      argv[0] !== "sc.exe"
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
      "sc.exe",
      "failure",
      name,
      "reset=",
      "86400",
      "actions=",
      "restart/5000/restart/30000/restart/60000",
    ]);
    // Recovery must also apply to clean Shawl exits that report a service error.
    await mutate(role, ["sc.exe", "failureflag", name, "1"]);
  };

  const resolveServiceAccount = async (
    account: string,
  ): Promise<{ sid: string; canonicalAccount: string }> => {
    assertWindowsOperatorAccount(account);
    const resolved = await runWindowsHelper<{ sid: string; canonicalAccount: string }>(
      "resolve-account-sid",
      [account],
    );
    if (!/^S-\d(?:-\d+)+$/.test(resolved.sid)) {
      throw new Error("Windows helper returned an invalid operator SID");
    }
    assertWindowsOperatorAccount(resolved.canonicalAccount);
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

  const applyServiceDacl = async (
    role: RoostServiceRole,
    sid: string,
  ): Promise<void> => {
    const name = serviceName(role);
    await query(role);
    const applied = await runWindowsHelper<{ sddl: string }>(
      "apply-service-dacl",
      [name, sid, "START,STOP,QUERY_STATUS,QUERY_CONFIG,CHANGE_CONFIG"],
    );
    const queried = await runChecked(["sc.exe", "sdshow", name]);
    if (!applied.sddl || !queried.stdout.replace(/\s+/g, "").includes(applied.sddl.replace(/\s+/g, ""))) {
      throw new Error(`${name} operator DACL did not round-trip through SCM`);
    }
    await query(role);
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
    const resolved = await provisionAccount(definition.account);
    const accountChanged = !sameAccount(before.account, resolved.canonicalAccount);
    if (accountChanged || credentials !== undefined) {
      if (!credentials) {
        throw new Error(`credential is required to assign ${definition.name} to ${resolved.canonicalAccount}`);
      }
      await configureServiceAccount(definition, credentials, resolved.canonicalAccount);
    }
    await mutate(definition.role, [
      "sc.exe",
      "config",
      definition.name,
      ...definitionConfigArgs(definition),
    ]);
    await mutate(definition.role, [
      "sc.exe",
      "description",
      definition.name,
      definition.description,
    ]);
    await configureRecovery(definition.role);
    await applyServiceDacl(definition.role, resolved.sid);
    const after = await query(definition.role);
    assertDefinitionApplied({ ...definition, account: resolved.canonicalAccount }, after);
    return after;
  };

  const install = async (
    definition: WindowsServiceDefinition,
    credentials?: WindowsServiceCredentials,
  ): Promise<WindowsServiceSnapshot> => {
    validateDefinition(definition);
    const before = await query(definition.role);
    if (before.installed) return configure(definition, credentials);
    if (!credentials) {
      throw new Error(`credential is required to install ${definition.name}`);
    }
    credentialPassword(definition, credentials, true);
    const dependencies = definition.dependencies.map(serviceName);
    // New services are inert until the helper has assigned the operator
    // account. No password or other secret ever appears in argv or env.
    await mutate(definition.role, [
      "sc.exe",
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
    return configure(definition, credentials);
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
    await mutate(role, ["sc.exe", "start", serviceName(role)]);
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
      await mutate(role, ["sc.exe", "stop", serviceName(role)]);
    }
    return waitForState(role, "stopped", stopOptions.timeoutMs ?? 30_000);
  };

  const restoreConfig = async (
    role: RoostServiceRole,
    saved: WindowsServiceSnapshot,
  ): Promise<void> => {
    if (saved.role !== role || saved.name !== serviceName(role)) {
      throw new Error(`invalid saved SCM snapshot for ${role}`);
    }
    const current = await query(role);
    if (!saved.installed) {
      if (!current.installed) return;
      if (role === "keeper" && current.state !== "stopped") {
        throw new Error("refusing to stop keeper while restoring an absent baseline");
      }
      if (current.state !== "stopped") await stop(role);
      await mutate(role, ["sc.exe", "delete", serviceName(role)]);
      return;
    }
    if (!current.installed) {
      throw new Error(`cannot restore deleted ${saved.name} without its operator credentials`);
    }
    if (saved.imagePath === null || saved.account === null || saved.startMode === "unknown") {
      throw new Error(`saved ${saved.name} configuration is incomplete`);
    }
    if (!sameAccount(current.account, saved.account)) {
      throw new Error(`cannot restore ${saved.name} account without explicit credentials`);
    }
    await mutate(role, [
      "sc.exe",
      "config",
      serviceName(role),
      "binPath=",
      saved.imagePath,
      "start=",
      START_MODE_SC_VALUE[saved.startMode],
      "depend=",
      saved.dependencies.length > 0 ? saved.dependencies.join("/") : "/",
    ]);
    const after = await query(role);
    if (
      after.imagePath !== saved.imagePath
      || after.startMode !== saved.startMode
      || !sameSet(after.dependencies, saved.dependencies)
    ) {
      throw new Error(`${saved.name} configuration did not restore exactly`);
    }
  };

  const restore = async (
    saved: WindowsServiceSnapshotSet,
    restoreOptions: WindowsServiceRestoreOptions = {},
  ): Promise<WindowsServiceSnapshotSet> => {
    for (const role of WINDOWS_SERVICE_ROLES) {
      await restoreConfig(role, saved[role]);
    }
    const lifecycleRoles = restoreOptions.restoreLifecycleRoles ?? ["worker", "coordinator"];
    for (const role of lifecycleRoles) {
      assertRole(role);
      const wanted = saved[role];
      if (!wanted.installed) continue;
      const current = await query(role);
      if (wanted.state === "running" && current.state !== "running") {
        await start(role);
      } else if (wanted.state === "stopped" && current.state !== "stopped") {
        if (role === "keeper") {
          throw new Error("refusing to stop keeper during SCM rollback");
        }
        await stop(role);
      }
    }
    return snapshot();
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
  };
}
