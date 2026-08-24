// Shared Windows SCM vocabulary: role allowlist, canonical service names,
// snapshot/definition/credential types, and the manager + native-runner
// contracts consumed by every windows/* module and the CLI installers.
//
// Callers: windows-service-definitions.ts, windows-service-scm.ts,
// windows-service-security.ts, windows-service-manager.ts, and (via the
// service-ctl.ts barrel) deploy/update/install entry points.
// Types only plus two frozen constants — no behavior lives here.

import type {
  WindowsServiceConfig,
  WindowsServiceRecoveryPolicy,
  WindowsServiceSidType,
} from "@roost/shared/windows-helper";

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
