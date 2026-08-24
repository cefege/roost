// Windows service-account and security provisioning: operator SID
// resolution, logon-as-service grants, service SID configuration, and the
// exact cross-service DACL matrix installed during elevated cutover.
//
// Callers: windows-service-manager.ts wires provisionServiceLogon /
// provisionServiceSecurity into the public manager and reuses provisionAccount
// during install/configure. Depends on windows-service-scm.ts (query/snapshot/
// configureServiceSidNative), windows-service-definitions.ts, and
// windows-identity.ts.

import {
  runWindowsHelper,
  windowsApplyServiceDacl,
  windowsResolveServiceSid,
  windowsRevokeServiceDacl,
  type WindowsServiceControlGrant,
  type WindowsServiceSnapshot as NativeWindowsServiceSnapshot,
} from "@roost/shared/windows-helper";
import { normalizedWindowsAccount } from "./windows-identity.ts";
import { sameAccount } from "./windows-service-scm.ts";
import {
  assertWindowsOperatorAccount,
  serviceName,
} from "./windows-service-definitions.ts";
import type { WindowsScmCore } from "./windows-service-scm.ts";
import {
  WINDOWS_SERVICE_ROLES,
  type RoostServiceRole,
} from "./windows-service-types.ts";

export interface WindowsServiceProvisioning {
  resolveServiceAccount(account: string): Promise<{
    sid: string;
    canonicalAccount: string;
    localAccount: boolean;
    administrator: boolean;
  }>;
  provisionAccount(account: string): Promise<{ sid: string; canonicalAccount: string }>;
  provisionServiceLogon(account: string): Promise<void>;
  provisionServiceSecurity(interactiveSid: string): Promise<void>;
}

export function createWindowsServiceProvisioning(core: Pick<
  WindowsScmCore,
  "query" | "snapshot" | "configureServiceSidNative"
>): WindowsServiceProvisioning {
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
    await core.query(role);
    const applied = await windowsApplyServiceDacl(serviceName(role), sid, rights);
    await assertServiceDaclRoundTrip(role, applied.sddl);
    await core.query(role);
  };

  const revokeServiceDacl = async (
    role: RoostServiceRole,
    sid: string,
  ): Promise<void> => {
    await core.query(role);
    const applied = await windowsRevokeServiceDacl(serviceName(role), sid);
    await assertServiceDaclRoundTrip(role, applied.sddl);
    await core.query(role);
  };

  const provisionServiceSecurity = async (interactiveSid: string): Promise<void> => {
    if (!/^S-1-(?:\d+-)+\d+$/.test(interactiveSid)) {
      throw new Error("Windows interactive operator SID is invalid");
    }
    const current = await core.snapshot();
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
      await core.configureServiceSidNative(serviceName(role), "unrestricted");
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

  return {
    resolveServiceAccount,
    provisionAccount,
    provisionServiceLogon,
    provisionServiceSecurity,
  };
}
