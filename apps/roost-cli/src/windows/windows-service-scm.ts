// Low-level Windows SCM primitives shared by every service mutation:
// native command execution, allowlist-gated sc.exe mutation with before/after
// query round-trips, snapshot queries, and lifecycle state waits.
//
// Callers: windows-service-manager.ts and windows-service-security.ts build
// on createWindowsScmCore(); nothing else may touch sc.exe directly.
// Depends on windows-service-types.ts, windows-identity.ts, and
// windows-service-definitions.ts (serviceName). Also hosts the pure
// case-folded comparison helpers used to verify SCM snapshot round-trips
// against desired definitions.

import { win32 } from "node:path";
import {
  resolveWindowsHelperPath,
  windowsConfigureService,
  windowsConfigureServiceSid,
  type WindowsServiceConfig,
  type WindowsServiceSnapshot as NativeWindowsServiceSnapshot,
  type WindowsServiceRecoveryPolicy,
  type WindowsServiceSidType,
} from "@roost/shared/windows-helper";
import {
  WINDOWS_SERVICE_NAMES,
  WINDOWS_SERVICE_ROLES,
  type RoostServiceRole,
  type WindowsNativeCommandResult,
  type WindowsServiceManagerOptions,
  type WindowsServiceQueryOptions,
  type WindowsServiceSnapshot,
  type WindowsServiceSnapshotSet,
  type WindowsServiceState,
} from "./windows-service-types.ts";
import { normalizedWindowsAccount } from "./windows-identity.ts";
import { serviceName } from "./windows-service-definitions.ts";

/** Case-folded account equality with machine-prefix normalization (SCM spellings vary). */
export function sameAccount(left: string | null, right: string): boolean {
  return left !== null && normalizedWindowsAccount(left) === normalizedWindowsAccount(right);
}

export function sameEnvironment(
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

export function isWindowsServiceSidType(value: unknown): value is WindowsServiceSidType {
  return value === "none" || value === "restricted" || value === "unrestricted";
}

export function sameRecoveryPolicy(
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

/** Everything the provisioning and lifecycle layers close over. */
export interface WindowsScmCore {
  readonly scPath: string;
  run(argv: readonly string[]): Promise<WindowsNativeCommandResult>;
  runChecked(argv: readonly string[]): Promise<WindowsNativeCommandResult>;
  configureNative(
    service: (typeof WINDOWS_SERVICE_NAMES)[RoostServiceRole],
    config: WindowsServiceConfig,
  ): Promise<unknown>;
  configureServiceSidNative(
    service: (typeof WINDOWS_SERVICE_NAMES)[RoostServiceRole],
    serviceSidType: WindowsServiceSidType,
  ): Promise<unknown>;
  query(role: RoostServiceRole, options?: WindowsServiceQueryOptions): Promise<WindowsServiceSnapshot>;
  snapshot(options?: WindowsServiceQueryOptions): Promise<WindowsServiceSnapshotSet>;
  mutate(role: RoostServiceRole, argv: readonly string[]): Promise<WindowsServiceSnapshot>;
  waitForState(
    role: RoostServiceRole,
    expected: WindowsServiceState,
    timeoutMs: number,
  ): Promise<WindowsServiceSnapshot>;
  start(role: RoostServiceRole): Promise<WindowsServiceSnapshot>;
  stop(role: RoostServiceRole, stopOptions?: { timeoutMs?: number }): Promise<WindowsServiceSnapshot>;
}

export function createWindowsScmCore(options: WindowsServiceManagerOptions = {}): WindowsScmCore {
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

  return {
    scPath,
    run,
    runChecked,
    configureNative,
    configureServiceSidNative,
    query,
    snapshot,
    mutate,
    waitForState,
    start,
    stop,
  };
}
