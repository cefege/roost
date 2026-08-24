// Public Windows service manager: composes the SCM core and security
// provisioning into createWindowsServiceManager() and owns the definition
// application choreography — install/configure round-trips, exact rollback
// of saved SCM snapshots, and the recovery-policy contract.
//
// Callers: windows-update-control.ts, windows-relocation-control.ts,
// install-binary-agents.ts, quickstart.ts, and tests via the service-ctl.ts
// barrel. Depends on windows-service-{types,definitions,scm,security}.ts.

import { runWindowsHelper } from "@roost/shared/windows-helper";
import type { WindowsServiceRecoveryPolicy } from "@roost/shared/windows-helper";
import { normalizedWindowsAccount } from "./windows-identity.ts";
import { createWindowsScmCore } from "./windows-service-scm.ts";
import { createWindowsServiceProvisioning } from "./windows-service-security.ts";
import {
  WINDOWS_SERVICE_ROLES,
  type RoostServiceRole,
  type WindowsServiceCredentials,
  type WindowsServiceDefinition,
  type WindowsServiceManager,
  type WindowsServiceManagerOptions,
  type WindowsServiceRestoreOptions,
  type WindowsServiceSnapshot,
  type WindowsServiceSnapshotSet,
} from "./windows-service-types.ts";
import {
  isWindowsServiceSidType,
  sameAccount,
  sameEnvironment,
  sameRecoveryPolicy,
} from "./windows-service-scm.ts";
import { sameSet, serviceName, validateDefinition } from "./windows-service-definitions.ts";

const START_MODE_SC_VALUE: Readonly<Record<"automatic" | "manual" | "disabled", string>> = {
  automatic: "auto",
  manual: "demand",
  disabled: "disabled",
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

function credentialPassword(
  definition: WindowsServiceDefinition,
  credentials: WindowsServiceCredentials | undefined,
  required: boolean,
): string | undefined {
  if (credentials && normalizedWindowsAccount(credentials.account) !== normalizedWindowsAccount(definition.account)) {
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
  const core = createWindowsScmCore(options);
  const provisioning = createWindowsServiceProvisioning(core);

  const configureServiceAccount = async (
    definition: WindowsServiceDefinition,
    credentials: WindowsServiceCredentials,
    canonicalAccount: string,
  ): Promise<void> => {
    const password = credentialPassword(definition, credentials, true);
    const input = password === undefined
      ? new Uint8Array()
      : new TextEncoder().encode(password);
    await core.query(definition.role);
    await runWindowsHelper<{ changed: boolean }>(
      "configure-service-account",
      [definition.name, canonicalAccount],
      { input, sensitive: true },
    );
    const after = await core.query(definition.role);
    if (!sameAccount(after.account, canonicalAccount)) {
      throw new Error(`${definition.name} operator account did not round-trip through SCM`);
    }
  };

  const configure = async (
    definition: WindowsServiceDefinition,
    credentials?: WindowsServiceCredentials,
  ): Promise<WindowsServiceSnapshot> => {
    validateDefinition(definition);
    const before = await core.query(definition.role);
    if (!before.installed) {
      throw new Error(`${definition.name} is not installed`);
    }
    const accountChanged = !sameAccount(before.account, definition.account);
    let canonicalAccount = before.account;
    if (accountChanged || credentials !== undefined) {
      if (!credentials) {
        throw new Error(`credential is required to assign ${definition.name} to ${definition.account}`);
      }
      const resolved = await provisioning.provisionAccount(definition.account);
      canonicalAccount = resolved.canonicalAccount;
      await configureServiceAccount(definition, credentials, canonicalAccount);
    }
    if (!canonicalAccount) throw new Error(`${definition.name} has no configured service account`);
    await core.mutate(definition.role, [
      core.scPath,
      "config",
      definition.name,
      ...definitionConfigArgs(definition),
    ]);
    await core.mutate(definition.role, [
      core.scPath,
      "description",
      definition.name,
      definition.description,
    ]);
    await configureRecovery(definition.role);
    const after = await core.query(definition.role);
    assertDefinitionApplied({ ...definition, account: canonicalAccount }, after);
    return after;
  };

  const install = async (
    definition: WindowsServiceDefinition,
    credentials?: WindowsServiceCredentials,
  ): Promise<WindowsServiceSnapshot> => {
    validateDefinition(definition);
    const before = await core.query(definition.role);
    if (before.installed) {
      if (!sameAccount(before.account, definition.account)) {
        throw new Error(`${definition.name} account migration cannot be rolled back without its prior credential`);
      }
      const configured = await configure(definition, credentials);
      await core.configureServiceSidNative(definition.name, "unrestricted");
      return configured;
    }
    if (!credentials) {
      throw new Error(`credential is required to install ${definition.name}`);
    }
    credentialPassword(definition, credentials, true);
    await provisioning.resolveServiceAccount(definition.account);
    const dependencies = definition.dependencies.map(serviceName);
    // New services are inert until the helper has assigned the operator
    // account. No password or other secret ever appears in argv or env.
    await core.mutate(definition.role, [
      core.scPath,
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
    await core.configureServiceSidNative(definition.name, "unrestricted");
    return configured;
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
    const current = await core.query(role);
    if (!saved.installed) {
      if (!current.installed) return;
      if (role === "keeper" && current.state !== "stopped" && !allowKeeperStop) {
        throw new Error("refusing to stop keeper while restoring an absent baseline");
      }
      if (current.state !== "stopped") await core.stop(role);
      await core.mutate(role, [core.scPath, "delete", serviceName(role)]);
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
    await core.mutate(role, [
      core.scPath,
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
      await core.configureServiceSidNative(serviceName(role), saved.serviceSidType);
    }
    await core.configureNative(serviceName(role), {
      description: saved.description,
      recoveryPolicy: saved.recoveryPolicy,
      ...(includeSecurity ? { securityDescriptor: saved.securityDescriptor! } : {}),
      ...(saved.environment !== undefined
        ? { environment: { ...saved.environment! } }
        : {}),
    });
    const after = await core.query(role, { includeSecurity });
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
      const current = await core.query(role);
      if (wanted.state === "running" && current.state !== "running") {
        await core.start(role);
      } else if (wanted.state === "stopped" && current.state !== "stopped") {
        if (role === "keeper" && !restoreOptions.allowKeeperStop) {
          throw new Error("refusing to stop keeper during SCM rollback");
        }
        await core.stop(role);
      }
    }
    return core.snapshot({ includeSecurity });
  };

  const configureRecovery = async (role: RoostServiceRole): Promise<void> => {
    const name = serviceName(role);
    await core.mutate(role, [
      core.scPath,
      "failure",
      name,
      "reset=",
      "86400",
      "actions=",
      "restart/5000/restart/30000/restart/60000",
    ]);
    // Recovery must also apply to clean Shawl exits that report a service error.
    await core.mutate(role, [core.scPath, "failureflag", name, "1"]);
  };

  return {
    query: core.query,
    snapshot: core.snapshot,
    install,
    configure,
    start: core.start,
    stop: core.stop,
    restore,
    provisionServiceLogon: provisioning.provisionServiceLogon,
    provisionServiceSecurity: provisioning.provisionServiceSecurity,
  };
}
