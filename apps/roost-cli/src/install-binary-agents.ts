// Install coord/worker services for the compiled binary. POSIX reuses the
// embedded launchd/systemd installers byte-for-byte. Windows uses the native
// allowlisted SCM manager and never invokes bash or a command-string shell.
import { COORD_INSTALL_SH, WORKER_INSTALL_SH } from "@roost/shared/install-scripts";
import { roostServiceDir } from "@roost/shared/paths";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { loadWorkerConfig } from "../../worker/src/config.ts";
import { createCoordClient } from "../../worker/src/coord-client.ts";
import { runStrictEnrollment } from "../../worker/src/install.ts";
import { loadWorkerKey, mintJwt } from "../../worker/src/jwt.ts";
import {
  WINDOWS_SERVICE_ROLES,
  buildWindowsServiceDefinitions,
  createWindowsServiceManager,
  storeWindowsServiceDefinitions,
  type RoostServiceRole,
  type WindowsServiceDefinition,
  type WindowsServiceCredentials,
} from "./service-ctl.ts";

type Cmd = "install" | "write-plist";

/**
 * Read the installer credential from one length-prefixed stdin frame. The
 * signed PowerShell front door writes: uint32-le byte length + UTF-8 password.
 * No secret is accepted through argv, environment, or disk.
 */
export async function readWindowsServiceCredentials(
  account = process.env.ROOST_SERVICE_ACCOUNT,
): Promise<WindowsServiceCredentials> {
  if (process.platform !== "win32") {
    throw new Error("framed Windows service credentials are only valid on Windows");
  }
  if (!account?.trim()) throw new Error("ROOST_SERVICE_ACCOUNT is required");
  const frame = new Uint8Array(await Bun.file(0).arrayBuffer());
  try {
    if (frame.byteLength < 4) throw new Error("missing framed Windows service credential");
    const length = new DataView(frame.buffer, frame.byteOffset, 4).getUint32(0, true);
    if (length === 0 || length > 16_384 || frame.byteLength !== length + 4) {
      throw new Error("invalid framed Windows service credential length");
    }
    const password = new TextDecoder("utf-8", { fatal: true }).decode(frame.subarray(4));
    return { account: account.trim(), password };
  } finally {
    frame.fill(0);
  }
}

function extractScript(name: string, body: string): string {
  if (!body) {
    throw new Error("embedded install scripts missing — build with scripts/build-binary.ts");
  }
  const dir = mkdtempSync(join(tmpdir(), "roost-agents-"));
  const path = join(dir, name);
  writeFileSync(path, body);
  return path;
}

async function runScript(path: string, cmd: Cmd, env: Record<string, string>): Promise<void> {
  const proc = Bun.spawn(["bash", path, cmd], {
    env: { ...process.env, ...env },
    stdio: ["ignore", "inherit", "inherit"],
  });
  if ((await proc.exited) !== 0) throw new Error(`${path} ${cmd} failed`);
}

function windowsEnvironment(extra: Record<string, string> = {}): Record<string, string> {
  return { ...(process.env as Record<string, string>), ...extra };
}

function requireWindowsValue(env: Record<string, string>, key: string): string {
  const value = env[key]?.trim();
  if (!value) throw new Error(`${key} is required for Windows service installation`);
  return value;
}

function windowsCredentials(
  env: Record<string, string>,
  credentials: WindowsServiceCredentials | undefined,
): WindowsServiceCredentials {
  const account = requireWindowsValue(env, "ROOST_SERVICE_ACCOUNT");
  if (!credentials) {
    throw new Error("a framed stdin credential is required for Windows service installation");
  }
  if (credentials.account.toLocaleLowerCase("en-US") !== account.toLocaleLowerCase("en-US")) {
    throw new Error("framed credential account does not match ROOST_SERVICE_ACCOUNT");
  }
  return credentials;
}

function publicRoleEnvironment(env: Record<string, string>): Record<string, string> {
  const blocked = new Set([
    "ROOST_SERVICE_ACCOUNT",
    "ROOST_SERVICE_PASSWORD",
    "ROOST_BOOTSTRAP_TOKEN",
    "ROOST_SHAWL_PATH",
    "ROOST_WIN_HELPER",
  ]);
  return Object.fromEntries(
    Object.entries(env).filter(([key]) => key.startsWith("ROOST_") && !blocked.has(key)),
  );
}

function windowsDefinitions(options: {
  execPath: string;
  coordinatorHost: boolean;
  env: Record<string, string>;
  credentials?: WindowsServiceCredentials;
  coordinatorEnvironment?: Record<string, string>;
  workerEnvironment?: Record<string, string>;
}): {
  definitions: Readonly<Record<RoostServiceRole, WindowsServiceDefinition>>;
  credentials: WindowsServiceCredentials;
} {
  const credentials = windowsCredentials(options.env, options.credentials);
  const releaseDir = dirname(options.execPath);
  const publisher = requireWindowsValue(options.env, "ROOST_WINDOWS_PUBLISHER_SHA256");
  if (!/^[0-9a-f]{64}$/i.test(publisher)) {
    throw new Error("ROOST_WINDOWS_PUBLISHER_SHA256 must be the pinned 64-hex signing leaf SHA-256");
  }
  const definitions = buildWindowsServiceDefinitions({
    executablePath: options.execPath,
    shawlPath: options.env.ROOST_SHAWL_PATH ?? join(releaseDir, "shawl.exe"),
    windowsHelperPath: options.env.ROOST_WIN_HELPER ?? join(releaseDir, "roost-win-helper.exe"),
    account: credentials.account,
    coordinatorHost: options.coordinatorHost,
    serviceDir: options.env.ROOST_SERVICE_DIR ?? roostServiceDir(undefined, "win32"),
    commonEnvironment: {
      ROOST_WINDOWS_PUBLISHER_SHA256: publisher.toUpperCase(),
    },
    roleEnvironment: {
      coordinator: options.coordinatorEnvironment ?? {},
      worker: options.workerEnvironment ?? {},
    },
  });
  return { definitions, credentials };
}

function prepareWindowsDirectories(
  definitions: Readonly<Record<RoostServiceRole, WindowsServiceDefinition>>,
): void {
  for (const definition of Object.values(definitions)) {
    mkdirSync(definition.cwd, { recursive: true });
    mkdirSync(definition.logDir, { recursive: true });
  }
}

function writeWindowsDryRun(
  definitions: Readonly<Record<RoostServiceRole, WindowsServiceDefinition>>,
  roles: readonly RoostServiceRole[],
  log: (message: string) => void,
): void {
  const dir = mkdtempSync(join(tmpdir(), "roost-dryrun-services-"));
  const path = join(dir, "services.json");
  writeFileSync(
    path,
    JSON.stringify(Object.fromEntries(roles.map((role) => [role, definitions[role]])), null, 2),
    "utf8",
  );
  log(`  dry-run Windows service definitions → ${path}`);
}

export async function installCoordAgent(opts: {
  execPath: string;
  gitSha: string;
  cmd?: Cmd;
  env?: Record<string, string>;
  credentials?: WindowsServiceCredentials;
  log: (m: string) => void;
}): Promise<void> {
  const cmd = opts.cmd ?? "install";
  opts.log(`installing coordinator service (roost coord)${cmd === "write-plist" ? " [dry-run]" : ""}`);

  switch (process.platform) {
    case "darwin":
    case "linux": {
      const script = extractScript("coord-install.sh", COORD_INSTALL_SH);
      const env: Record<string, string> = {
        ROOST_EXEC_BIN: opts.execPath,
        ROOST_WORKDIR: homedir(),
        ROOST_GIT_SHA: opts.gitSha,
        ...opts.env,
      };
      if (cmd === "write-plist") {
        const dir = mkdtempSync(join(tmpdir(), "roost-dryrun-coord-"));
        env.ROOST_COORD_LABEL = "com.roost.coordinator-dryrun";
        env.ROOST_COORD_PLIST = join(dir, "coord.plist");
        env.ROOST_COORD_UNIT = join(dir, "coord.service");
        env.ROOST_COORD_DATA_DIR = join(dir, "data");
        env.ROOST_COORD_LOG_DIR = join(dir, "logs");
        opts.log(`  dry-run service definition → ${process.platform === "darwin" ? env.ROOST_COORD_PLIST : env.ROOST_COORD_UNIT}`);
      }
      await runScript(script, cmd, env);
      return;
    }
    case "win32": {
      const env = windowsEnvironment(opts.env);
      const credentials = opts.credentials ?? (cmd === "write-plist"
        ? { account: requireWindowsValue(env, "ROOST_SERVICE_ACCOUNT"), password: "" }
        : undefined);
      const context = windowsDefinitions({
        execPath: opts.execPath,
        coordinatorHost: true,
        env,
        credentials,
        coordinatorEnvironment: {
          ...publicRoleEnvironment(opts.env ?? {}),
          ROOST_GIT_SHA: opts.gitSha,
        },
      });
      if (cmd === "write-plist") {
        writeWindowsDryRun(context.definitions, ["coordinator", "updater"], opts.log);
        return;
      }
      prepareWindowsDirectories(context.definitions);
      const manager = createWindowsServiceManager();
      await manager.install(context.definitions.coordinator, context.credentials);
      await manager.install(context.definitions.updater, context.credentials);
      await manager.start("coordinator");
      return;
    }
    default:
      throw new Error(`unsupported service platform: ${process.platform}`);
  }
}

export async function installWorkerAgent(opts: {
  execPath: string;
  coordUrl: string;
  bootstrapToken?: string;
  gitSha: string;
  cmd?: Cmd;
  env?: Record<string, string>;
  credentials?: WindowsServiceCredentials;
  coordinatorHost?: boolean;
  coordinatorEnvironment?: Record<string, string>;
  log: (m: string) => void;
}): Promise<void> {
  const cmd = opts.cmd ?? "install";
  opts.log(`installing worker service (roost worker)${cmd === "write-plist" ? " [dry-run]" : ""}`);

  switch (process.platform) {
    case "darwin":
    case "linux": {
      const script = extractScript("worker-install.sh", WORKER_INSTALL_SH);
      const env: Record<string, string> = {
        ROOST_EXEC_BIN: opts.execPath,
        ROOST_WORKDIR: homedir(),
        ROOST_COORDINATOR_URL: opts.coordUrl,
        GIT_SHA: opts.gitSha,
        ...(opts.bootstrapToken ? { ROOST_BOOTSTRAP_TOKEN: opts.bootstrapToken } : {}),
      };
      if (cmd === "write-plist") {
        const dir = mkdtempSync(join(tmpdir(), "roost-dryrun-worker-"));
        env.ROOST_WORKER_AGENT_LABEL = "com.roost.worker-dryrun";
        env.ROOST_WORKER_PLIST = join(dir, "worker.plist");
        env.ROOST_WORKER_UNIT = join(dir, "worker.service");
        env.ROOST_WORKER_DATA_DIR = join(dir, "data");
        env.ROOST_WORKER_LOG_DIR = join(dir, "logs");
        opts.log(`  dry-run service definition → ${process.platform === "darwin" ? env.ROOST_WORKER_PLIST : env.ROOST_WORKER_UNIT}`);
      }
      await runScript(script, cmd, env);
      return;
    }
    case "win32": {
      const env = windowsEnvironment(opts.env);
      const credentials = opts.credentials ?? (cmd === "write-plist"
        ? { account: requireWindowsValue(env, "ROOST_SERVICE_ACCOUNT"), password: "" }
        : undefined);
      const workerEnvironment: Record<string, string> = {
        ...publicRoleEnvironment(opts.env ?? {}),
        ROOST_COORDINATOR_URL: opts.coordUrl,
        GIT_SHA: opts.gitSha,
      };
      const context = windowsDefinitions({
        execPath: opts.execPath,
        coordinatorHost: opts.coordinatorHost ?? false,
        env,
        credentials,
        coordinatorEnvironment: opts.coordinatorEnvironment,
        workerEnvironment,
      });
      if (cmd === "write-plist") {
        writeWindowsDryRun(context.definitions, WINDOWS_SERVICE_ROLES, opts.log);
        return;
      }
      prepareWindowsDirectories(context.definitions);
      if (opts.bootstrapToken) {
        const enrollmentEnv: Record<string, string | undefined> = {
          ...(process.env as Record<string, string | undefined>),
          ...workerEnvironment,
          ROOST_WORKER_DATA_DIR: context.definitions.worker.environment.ROOST_WORKER_DATA_DIR,
          ROOST_BOOTSTRAP_TOKEN: opts.bootstrapToken,
        };
        try {
          const cfg = loadWorkerConfig(enrollmentEnv);
          const key = await loadWorkerKey(cfg.workerKeyPath);
          const client = createCoordClient({
            cfg,
            getJwt: () => mintJwt(key, "roost-coordinator"),
          });
          await runStrictEnrollment({ cfg, client });
        } finally {
          delete enrollmentEnv.ROOST_BOOTSTRAP_TOKEN;
        }
      }
      const manager = createWindowsServiceManager();
      for (const role of WINDOWS_SERVICE_ROLES) {
        await manager.install(context.definitions[role], context.credentials);
      }
      await storeWindowsServiceDefinitions(context.definitions);
      if (opts.coordinatorHost) {
        await manager.start("coordinator");
      } else {
        const coordinator = await manager.query("coordinator");
        if (coordinator.state !== "stopped") await manager.stop("coordinator");
      }
      await manager.start("keeper");
      await manager.start("worker");
      return;
    }
    default:
      throw new Error(`unsupported service platform: ${process.platform}`);
  }
}
