import { readFileSync } from "node:fs";
import { win32 } from "node:path";
import type { WindowsServiceSnapshot } from "./windows-helper.ts";

export const WINDOWS_RELOCATION_SCHEMA_VERSION = 1 as const;

export type WindowsRelocationOperationKind =
  | "worker-endpoint"
  | "coordinator-promotion";

export type WindowsRelocationCommandAction =
  | "START"
  | "APPLY"
  | "STATUS"
  | "COMMIT"
  | "RESTORE";

/** Canonical machine paths supplied by the Worker and independently derived and
 * checked by RoostUpdaterV2 before it may publish a role override. */
export interface WindowsCoordinatorRelocationPaths {
  installRoot: string;
  serviceDir: string;
  versionsDir: string;
  serviceDefinitionsPath: string;
  coordinatorDataDir: string;
  coordinatorLogDir: string;
  coordinatorDbPath: string;
  coordinatorKeyPath: string;
  coordinatorAuthorizedKeysPath: string;
  coordinatorHandoffPath: string;
}

interface WindowsRelocationOperationBase {
  schemaVersion: typeof WINDOWS_RELOCATION_SCHEMA_VERSION;
  relocationId: string;
  handoffId: string;
  sourceUrl: string;
  targetUrl: string;
  /** Exact native helper snapshot observed while the relocation lock is held. */
  expectedBefore: WindowsServiceSnapshot;
}

export interface WindowsWorkerEndpointRelocationOperation
  extends WindowsRelocationOperationBase {
  kind: "worker-endpoint";
}

export interface WindowsCoordinatorPromotionRelocationOperation
  extends WindowsRelocationOperationBase {
  kind: "coordinator-promotion";
  expectedGitSha: string;
  paths: WindowsCoordinatorRelocationPaths;
}

/** Deliberately closed union: this is not a generic privileged request. */
export type WindowsRelocationOperation =
  | WindowsWorkerEndpointRelocationOperation
  | WindowsCoordinatorPromotionRelocationOperation;

export interface WindowsRelocationBrokerCommand {
  schemaVersion: typeof WINDOWS_RELOCATION_SCHEMA_VERSION;
  requestId: string;
  relocationId: string;
  handoffId: string;
  operationKind: WindowsRelocationOperationKind;
  action: WindowsRelocationCommandAction;
  /** Present exactly for START; later commands are bound to the durable journal. */
  operation?: WindowsRelocationOperation;
  afterRevision?: number;
}

export interface WindowsRelocationResultFrame {
  requestId: string;
  relocationId: string;
  handoffId: string;
  operationKind: WindowsRelocationOperationKind;
  revision: number;
  phase: string;
  message: string;
  terminal: boolean;
  success: boolean;
  error: string;
}

export type WindowsRelocationOverrideRole = "worker" | "coordinator";

export interface WindowsRelocationRoleOverride {
  schemaVersion: typeof WINDOWS_RELOCATION_SCHEMA_VERSION;
  role: WindowsRelocationOverrideRole;
  relocationId: string;
  handoffId: string;
  environment: Readonly<Record<string, string>>;
}

export function windowsRelocationOverridePath(
  serviceDir: string,
  role: WindowsRelocationOverrideRole,
): string {
  return win32.join(serviceDir, "data", "updater", "relocation", `${role}.json`);
}

/** Read-only consumer used before loading the Worker/Coordinator runtime. The
 * updater validates and protects the artifact; consumers still reject shape
 * expansion so a future writer cannot accidentally create a generic env file. */
export function readWindowsRelocationRoleOverride(
  serviceDir: string,
  role: WindowsRelocationOverrideRole,
): WindowsRelocationRoleOverride | null {
  let raw: string;
  try {
    raw = readFileSync(windowsRelocationOverridePath(serviceDir, role), "utf8");
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return null;
    throw error;
  }
  const value = JSON.parse(raw) as Partial<WindowsRelocationRoleOverride>;
  const keys = Object.keys(value).sort();
  if (
    JSON.stringify(keys) !== JSON.stringify([
      "environment", "handoffId", "relocationId", "role", "schemaVersion",
    ])
    || value.schemaVersion !== WINDOWS_RELOCATION_SCHEMA_VERSION
    || value.role !== role
    || typeof value.relocationId !== "string"
    || typeof value.handoffId !== "string"
    || !value.environment
    || Array.isArray(value.environment)
  ) {
    throw new Error(`invalid Windows ${role} relocation override`);
  }
  const environment = value.environment as Record<string, unknown>;
  const allowed = role === "worker"
    ? ["ROOST_COORDINATOR_URL"]
    : [
      "ROOST_COORDINATOR_AUTHORIZED_KEYS",
      "ROOST_COORDINATOR_DB",
      "ROOST_COORDINATOR_HANDOFF_PATH",
      "ROOST_COORDINATOR_KEY_PATH",
      "ROOST_COORDINATOR_PUBLIC_URL",
      "ROOST_COORD_DATA_DIR",
      "ROOST_COORD_LOG_DIR",
      "ROOST_GIT_SHA",
      "ROOST_LOG_ENCODING",
      "ROOST_SKIP_ENV_LOCAL",
    ];
  if (
    JSON.stringify(Object.keys(environment).sort()) !== JSON.stringify([...allowed].sort())
    || Object.values(environment).some((entry) => typeof entry !== "string" || /[\0\r\n]/.test(entry))
  ) {
    throw new Error(`invalid Windows ${role} relocation environment`);
  }
  return {
    schemaVersion: WINDOWS_RELOCATION_SCHEMA_VERSION,
    role,
    relocationId: value.relocationId,
    handoffId: value.handoffId,
    environment: environment as Record<string, string>,
  };
}
