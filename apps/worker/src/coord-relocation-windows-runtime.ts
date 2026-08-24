// Concrete WindowsCoordRuntime: executes SCM queries and relocation broker
// commands through @roost/shared/windows-helper, maps service state into the
// snapshots and recovery policies the Windows relocation operation expects,
// and reports host arch/release alongside results for fleet diagnostics.
import { arch, release } from "node:os";
import type {
  WindowsRelocationBrokerCommand,
  WindowsRelocationResultFrame,
} from "@roost/shared/windows-relocation";
import {
  windowsQueryService,
  type WindowsServiceRecoveryPolicy,
  type WindowsServiceSnapshot as NativeWindowsServiceSnapshot,
  type WindowsServiceSnapshotWithSecurity as NativeWindowsServiceSnapshotWithSecurity,
  type WindowsServiceState as NativeWindowsServiceState,
} from "@roost/shared/windows-helper";

export const WINDOWS_SERVICE_NAMES = {
  keeper: "RoostKeeperV2",
  worker: "RoostWorkerV2",
  coordinator: "RoostCoordinatorV2",
  updater: "RoostUpdaterV2",
} as const;

export type WindowsServiceRole = keyof typeof WINDOWS_SERVICE_NAMES;
export type WindowsServiceName = (typeof WINDOWS_SERVICE_NAMES)[WindowsServiceRole];
export type WindowsServiceState = "stopped" | "start-pending" | "stop-pending" | "running";
export type WindowsServiceStartMode = "automatic" | "manual" | "disabled";

/** Read-only service state retained in the Worker's local relocation journal. */
export interface WindowsServiceSnapshot {
  role: WindowsServiceRole;
  name: WindowsServiceName;
  installed: boolean;
  state: WindowsServiceState;
  startMode: WindowsServiceStartMode;
  imagePath: string;
  imageArgv: string[];
  account: string;
  dependencies: string[];
  environment: Record<string, string>;
  shawlPath: string;
  shawlArguments: string[];
  executablePath: string;
  arguments: string[];
  cwd: string;
  logDir: string;
  displayName: string;
  description: string;
  recoveryPolicy: WindowsServiceRecoveryPolicy;
  securityDescriptor: string;
}

/** Worker capability surface: read-only inspection plus updater admission. */
export interface WindowsCoordRuntime {
  readonly platform: string;
  readonly arch: string;
  readonly release: string;
  queryService(role: WindowsServiceRole): Promise<WindowsServiceSnapshot>;
  queryRelocationService(role: WindowsServiceRole): Promise<NativeWindowsServiceSnapshot>;
  runRelocationCommand(
    command: WindowsRelocationBrokerCommand,
    beforeUpdaterStart: () => Promise<void>,
  ): Promise<WindowsRelocationResultFrame>;
}

function serviceName(role: WindowsServiceRole): WindowsServiceName {
  return WINDOWS_SERVICE_NAMES[role];
}

function relocationServiceState(state: NativeWindowsServiceState): WindowsServiceState {
  switch (state) {
    case "stopped":
    case "start-pending":
    case "stop-pending":
    case "running":
      return state;
    default:
      throw new Error(`Roost service is in unsupported SCM state ${state}`);
  }
}

function optionValue(argv: readonly string[], option: string): string {
  const index = argv.indexOf(option);
  return index >= 0 ? (argv[index + 1] ?? "") : "";
}

function relocationServiceSnapshot(
  role: WindowsServiceRole,
  snapshot: NativeWindowsServiceSnapshotWithSecurity,
): WindowsServiceSnapshot {
  const expected = serviceName(role);
  if (snapshot.name !== expected) {
    throw new Error(`Windows helper returned service ${snapshot.name} for ${expected}`);
  }
  const imageArgv = [...snapshot.binaryArgv];
  const shawlPath = imageArgv[0] ?? "";
  const shawlArguments = imageArgv.slice(1);
  const separator = shawlArguments.indexOf("--");
  return {
    role,
    name: expected,
    installed: true,
    state: relocationServiceState(snapshot.state),
    startMode: snapshot.startType,
    imagePath: snapshot.imagePathRaw,
    imageArgv,
    account: snapshot.account,
    dependencies: [...snapshot.dependencies],
    environment: { ...snapshot.environment },
    shawlPath,
    shawlArguments,
    executablePath: separator >= 0 ? (shawlArguments[separator + 1] ?? "") : "",
    arguments: separator >= 0 ? shawlArguments.slice(separator + 2) : [],
    cwd: optionValue(shawlArguments, "--cwd"),
    logDir: optionValue(shawlArguments, "--log-dir"),
    displayName: snapshot.displayName,
    description: snapshot.description,
    recoveryPolicy: {
      ...snapshot.recoveryPolicy,
      actions: snapshot.recoveryPolicy.actions.map((action) => ({ ...action })),
    },
    securityDescriptor: snapshot.securityDescriptor,
  };
}

/** Default adapter has no authoritative machine-mutation methods. */
export function createDefaultWindowsCoordRuntime(): WindowsCoordRuntime {
  return {
    platform: process.platform,
    arch: arch(),
    release: release(),
    async queryService(role) {
      return relocationServiceSnapshot(role, await windowsQueryService(serviceName(role)));
    },
    async queryRelocationService(role) {
      return await windowsQueryService(serviceName(role));
    },
    async runRelocationCommand(command, beforeUpdaterStart) {
      const { executeWindowsRelocationBrokerCommand } = await import(
        "../../roost-cli/src/windows/windows-relocation-control.ts"
      );
      return await executeWindowsRelocationBrokerCommand(command, { beforeUpdaterStart });
    },
  };
}
