// Windows service-definition construction, validation, and persistence:
// the four-role Shawl topology builder, updater retargeting, structural
// validation, and the protected service-definitions.json store/load pair.
//
// Callers: windows-service-manager.ts (SCM mutations validate every
// definition here), windows-update-control.ts, install-binary-agents.ts,
// quickstart.ts, deploy.ts, and tests via the service-ctl.ts barrel.
// Depends on windows-service-types.ts and windows-identity.ts only.

import { durableWriteFile } from "@roost/shared/durability";
import { roostServiceDir } from "@roost/shared/paths";
import { windowsReplaceUpdaterArtifact } from "@roost/shared/windows-helper";
import { dirname, join } from "node:path";
import { normalizedWindowsAccount } from "./windows-identity.ts";
import {
  WINDOWS_SERVICE_NAMES,
  WINDOWS_SERVICE_ROLES,
  type RoostServiceRole,
  type WindowsServiceDefinition,
  type WindowsServiceDefinitionOptions,
} from "./windows-service-types.ts";

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

function assertRole(value: string): asserts value is RoostServiceRole {
  if (!(WINDOWS_SERVICE_ROLES as readonly string[]).includes(value)) {
    throw new Error(`Windows service role is not allowlisted: ${value}`);
  }
}

function assertNoCommandControl(value: string, label: string): void {
  if (!value || /[\0\r\n]/.test(value)) {
    throw new Error(`${label} must be non-empty and contain no NUL or newline`);
  }
}

export function assertWindowsOperatorAccount(account: string): void {
  assertNoCommandControl(account, "Windows service account");
  const normalized = normalizedWindowsAccount(account);
  // Normalization strips any machine prefix, so ".\LocalSystem" hits the bare entry.
  if (["localsystem", "system", "nt authority\\system"].includes(normalized)) {
    throw new Error("Roost Windows services require an explicit operator account; LocalSystem is forbidden");
  }
}

export function serviceName(role: RoostServiceRole): (typeof WINDOWS_SERVICE_NAMES)[RoostServiceRole] {
  assertRole(role);
  return WINDOWS_SERVICE_NAMES[role];
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

export function sameSet(left: readonly string[], right: readonly string[]): boolean {
  const a = [...left].map((value) => value.toLocaleLowerCase("en-US")).sort();
  const b = [...right].map((value) => value.toLocaleLowerCase("en-US")).sort();
  return a.length === b.length && a.every((value, index) => value === b[index]);
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
    "run", "--name", name, "--no-restart", "--kill-process-tree",
    "--stop-timeout", "15000", "--cwd", options.cwd,
    "--log-dir", options.logDir, "--log-as", role,
    "--log-cmd-as", `${role}-stdio`,
    "--log-rotate", "bytes=2097152", "--log-retain", "2",
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
  let parsed: { schemaVersion?: unknown; services?: Partial<Record<RoostServiceRole, WindowsServiceDefinition>> };
  try {
    parsed = JSON.parse(raw);
  } catch {
    // A torn write surfaces as a SyntaxError; recovery is a clean rewrite.
    throw new Error("Windows service-definitions.json is malformed; re-run the signed elevated Roost installer to rewrite it");
  }
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

export function validateDefinition(definition: WindowsServiceDefinition): void {
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
