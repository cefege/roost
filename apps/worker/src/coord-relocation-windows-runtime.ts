import * as fs from "node:fs";
import { arch, release } from "node:os";
import { join } from "node:path";
import { randomUUID } from "node:crypto";
import {
  applyPrivateDacl,
  durableRemove,
  durableReplace,
  durableWriteFile,
  flushDurablePath,
} from "@roost/shared/durability";
import {
  windowsApplySddl,
  windowsConfigureService,
  windowsProbeExclusiveOpen,
  windowsQueryService,
  windowsReadDacl,
  windowsStartService,
  windowsStopService,
  type WindowsServiceRecoveryPolicy,
  type WindowsServiceSnapshot as NativeWindowsServiceSnapshot,
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

/** Exact, round-trippable SCM + service-registry state. */
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

export interface WindowsDaclSnapshot {
  sddl: string;
}

export interface WindowsCoordRuntime {
  readonly platform: string;
  readonly arch: string;
  readonly release: string;
  queryService(role: WindowsServiceRole): Promise<WindowsServiceSnapshot>;
  configureService(snapshot: WindowsServiceSnapshot): Promise<void>;
  startService(role: WindowsServiceRole): Promise<void>;
  stopService(role: WindowsServiceRole, timeoutMs: number): Promise<void>;
  probeExclusiveOpen(path: string): Promise<boolean>;
  readDacl(path: string): Promise<WindowsDaclSnapshot>;
  applySddl(path: string, sddl: string): Promise<void>;
  applyPrivateDacl(path: string): Promise<void>;
  durableWrite(path: string, data: string | Uint8Array, mode?: number): Promise<void>;
  durableReplace(source: string, destination: string): Promise<void>;
  durableRemove(path: string): Promise<void>;
  flush(path: string): Promise<void>;
  captureTailscaleConfig(stagingDir: string): Promise<string>;
  configureTailscaleServe(): Promise<void>;
  restoreTailscaleConfig(config: string, stagingDir: string): Promise<void>;
  tailscaleReady(): Promise<void>;
  coordinatorHealthy(targetUrl: string): Promise<boolean>;
}

interface CommandResult {
  stdout: string;
  stderr: string;
  exitCode: number;
}

async function run(argv: string[], timeoutMs = 15_000): Promise<CommandResult> {
  const child = Bun.spawn(argv, {
    stdout: "pipe",
    stderr: "pipe",
    signal: AbortSignal.timeout(timeoutMs),
  });
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ]);
  return { stdout, stderr, exitCode };
}

function commandError(operation: string, result: CommandResult): Error {
  const detail = `${result.stderr}\n${result.stdout}`.trim().split("\n").filter(Boolean).slice(-4).join("; ");
  return new Error(`${operation} failed (exit ${result.exitCode})${detail ? `: ${detail}` : ""}`);
}

function tailscaleBin(): string {
  return process.env.ROOST_TAILSCALE_BIN ?? "tailscale.exe";
}

function serviceName(role: WindowsServiceRole): WindowsServiceName {
  return WINDOWS_SERVICE_NAMES[role];
}

function assertServiceIdentity(role: WindowsServiceRole, snapshot: WindowsServiceSnapshot): WindowsServiceSnapshot {
  const expected = serviceName(role);
  if (snapshot.role !== role || snapshot.name !== expected) {
    throw new Error(`Windows helper returned service ${snapshot.name} for ${expected}`);
  }
  return snapshot;
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
  snapshot: NativeWindowsServiceSnapshot,
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


/** Default adapter: typed TypeScript orchestration over narrowly allowlisted native operations. */
export function createDefaultWindowsCoordRuntime(): WindowsCoordRuntime {
  return {
    platform: process.platform,
    arch: arch(),
    release: release(),
    async queryService(role) {
      return relocationServiceSnapshot(role, await windowsQueryService(serviceName(role)));
    },
    async configureService(snapshot) {
      assertServiceIdentity(snapshot.role, snapshot);
      await windowsConfigureService(snapshot.name, {
        binaryArgv: snapshot.imageArgv,
        startType: snapshot.startMode,
        dependencies: snapshot.dependencies,
        environment: snapshot.environment,
        displayName: snapshot.displayName,
        description: snapshot.description,
        recoveryPolicy: snapshot.recoveryPolicy,
        securityDescriptor: snapshot.securityDescriptor,
      });
    },
    async startService(role) {
      const result = await windowsStartService(serviceName(role));
      if (result.state !== "running") throw new Error(`${result.name} did not reach SCM RUNNING`);
    },
    async stopService(role, timeoutMs) {
      if (role === "keeper") throw new Error("coordinator relocation must never stop RoostKeeperV2");
      const result = await windowsStopService(serviceName(role), timeoutMs);
      if (result.state !== "stopped") throw new Error(`${result.name} did not reach SCM STOPPED`);
    },
    probeExclusiveOpen: windowsProbeExclusiveOpen,
    readDacl: windowsReadDacl,
    applySddl: windowsApplySddl,
    async applyPrivateDacl(path) {
      await applyPrivateDacl(path, { platform: "win32" });
    },
    async durableWrite(path, data, mode) {
      await durableWriteFile(path, data, { platform: "win32", mode, privateDacl: true });
    },
    async durableReplace(source, destination) {
      await durableReplace(source, destination, { platform: "win32", privateDacl: true });
    },
    async durableRemove(path) {
      await durableRemove(path, { platform: "win32", privateDacl: true });
    },
    async flush(path) {
      await flushDurablePath(path, { platform: "win32" });
    },
    async captureTailscaleConfig(stagingDir) {
      const path = join(stagingDir, `.tailscale-${randomUUID()}.json`);
      try {
        const result = await run([tailscaleBin(), "serve", "get-config", path, "--all"]);
        if (result.exitCode !== 0) throw commandError("tailscale serve get-config", result);
        return fs.existsSync(path) ? fs.readFileSync(path, "utf8") : "";
      } finally {
        fs.rmSync(path, { force: true });
      }
    },
    async configureTailscaleServe() {
      if (process.env.ROOST_FRONTED === "0") return;
      const httpsPort = process.env.ROOST_TAILNET_HTTPS_PORT ?? "4102";
      const loopbackPort = process.env.ROOST_COORD_LOOPBACK_PORT ?? "4103";
      const result = await run([
        tailscaleBin(), "serve", "--bg", `--https=${httpsPort}`, `http://127.0.0.1:${loopbackPort}`,
      ]);
      if (result.exitCode !== 0) throw commandError("tailscale serve", result);
    },
    async restoreTailscaleConfig(config, stagingDir) {
      const trimmed = config.trim();
      if (!trimmed || trimmed === "{}" || trimmed === "null") {
        const reset = await run([tailscaleBin(), "serve", "reset"]);
        if (reset.exitCode !== 0) throw commandError("tailscale serve reset", reset);
        return;
      }
      const path = join(stagingDir, `.tailscale-restore-${randomUUID()}.json`);
      try {
        await durableWriteFile(path, config, { platform: "win32", mode: 0o600, privateDacl: true });
        const result = await run([tailscaleBin(), "serve", "set-config", path, "--all"]);
        if (result.exitCode !== 0) throw commandError("tailscale serve set-config", result);
      } finally {
        await durableRemove(path, { platform: "win32", privateDacl: true }).catch(() => {});
      }
    },
    async tailscaleReady() {
      const result = await run([tailscaleBin(), "status", "--json"]);
      if (result.exitCode !== 0) throw commandError("tailscale status", result);
      let status: { BackendState?: string; Self?: { DNSName?: string } };
      try {
        status = JSON.parse(result.stdout) as typeof status;
      } catch {
        throw new Error("tailscale status returned invalid JSON");
      }
      if (status.BackendState !== "Running" || !status.Self?.DNSName?.toLowerCase().endsWith(".ts.net.")) {
        throw new Error("Tailscale is not connected with a tailnet DNS name");
      }
    },
    async coordinatorHealthy(targetUrl) {
      const response = await fetch(`${targetUrl.replace(/\/$/, "")}/roost.v1.CoordinatorService/MiscHealth`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: "{}",
        signal: AbortSignal.timeout(5_000),
      }).catch(() => null);
      return response?.ok === true;
    },
  };
}
