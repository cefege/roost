// Install coord/worker services for the compiled binary. POSIX reuses the
// embedded launchd/systemd installers byte-for-byte. Windows uses the native
// allowlisted SCM manager and never invokes bash or a command-string shell.
import { COORD_INSTALL_SH, WORKER_INSTALL_SH } from "@roost/shared/install-scripts";
import { roostServiceDir } from "@roost/shared/paths";
import {
  runWindowsHelper,
  windowsProtectUpdaterArtifact,
} from "@roost/shared/windows-helper";
import { durableWriteFile } from "@roost/shared/durability";
import { normalizedWindowsAccount } from "./windows/windows-identity.ts";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { homedir, tmpdir } from "node:os";
import { dirname, join, win32 } from "node:path";
import { loadWorkerConfig } from "../../worker/src/config.ts";
import { createCoordClient } from "../../worker/src/coord-client.ts";
import { runStrictEnrollment } from "../../worker/src/install.ts";
import { loadWorkerKey, mintJwt } from "../../worker/src/jwt.ts";
import {
  WINDOWS_SERVICE_ROLES,
  buildWindowsServiceDefinitions,
  createWindowsServiceManager,
  storeWindowsServiceDefinitions,
  windowsServiceDefinitionsPath,
  type RoostServiceRole,
  type WindowsServiceDefinition,
  type WindowsServiceCredentials,
  type WindowsServiceManager,
  type WindowsServiceSnapshotSet,
} from "./service-ctl.ts";

type Cmd = "install" | "write-plist";
export const WINDOWS_ROLE_STATE_PROFILES = [
  "keeper-state",
  "worker-state",
  "coordinator-state",
  "updater-state",
] as const;

export type WindowsRoleStateProfile = (typeof WINDOWS_ROLE_STATE_PROFILES)[number];

export interface WindowsRoleStateTreeOptions {
  account: string;
  interactiveSid: string;
  helperPath?: string;
}

export interface WindowsRoleStateTreeProtection {
  protected: true;
  profile: WindowsRoleStateProfile;
  directories: number;
  files: number;
}

export interface WindowsFileSecurityTreeEntry {
  /** Empty for the root, otherwise a root-relative path with native separators. */
  relativePath: string;
  kind: "file" | "directory";
  linkCount: number;
  /** Complete owner, protected-DACL control, and ordered ACE SDDL. */
  sddl: string;
}

export interface WindowsFileSecurityTreeSnapshot {
  root: string;
  entries: readonly WindowsFileSecurityTreeEntry[];
}

export interface WindowsFilesystemSecurityOptions {
  helperPath?: string;
}

/**
 * Harden an existing role-owned state tree through handles opened without
 * following reparses. The native helper rejects every reparse point/hardlink
 * and verifies the owner and protected DACL on every object before returning.
 */
export async function protectWindowsRoleStateTree(
  path: string,
  profile: WindowsRoleStateProfile,
  options: WindowsRoleStateTreeOptions,
): Promise<WindowsRoleStateTreeProtection> {
  if (!(WINDOWS_ROLE_STATE_PROFILES as readonly string[]).includes(profile)) {
    throw new Error(`Windows role-state profile is not allowlisted: ${profile}`);
  }
  if (!options.account.trim()) throw new Error("Windows role-state account is required");
  if (!/^S-1-(?:\d+-)+\d+$/.test(options.interactiveSid)) {
    throw new Error("Windows role-state interactive SID is invalid");
  }
  const result = await runWindowsHelper<WindowsRoleStateTreeProtection>(
    "protect-directory-tree",
    [path, profile, options.account, options.interactiveSid],
    { helperPath: options.helperPath },
  );
  if (
    result.protected !== true
    || result.profile !== profile
    || !Number.isSafeInteger(result.directories)
    || result.directories < 1
    || !Number.isSafeInteger(result.files)
    || result.files < 0
  ) {
    throw new Error("Windows role-state tree protection returned an invalid proof");
  }
  return Object.freeze({ ...result });
}

function checkedWindowsFileSecurityTreeEntries(
  entries: readonly WindowsFileSecurityTreeEntry[],
): readonly WindowsFileSecurityTreeEntry[] {
  if (!Array.isArray(entries) || entries.length === 0 || entries[0]?.relativePath !== "") {
    throw new Error("Windows filesystem security snapshot is missing its root descriptor");
  }
  const seen = new Set<string>();
  return Object.freeze(entries.map((entry) => {
    if (
      typeof entry.relativePath !== "string"
      || entry.relativePath.includes("\0")
      || entry.relativePath.startsWith("\\")
      || /^[A-Za-z]:/.test(entry.relativePath)
      || entry.relativePath.split(/[\\/]/).includes("..")
      || (entry.kind !== "file" && entry.kind !== "directory")
      || !Number.isSafeInteger(entry.linkCount)
      || entry.linkCount < 1
      || typeof entry.sddl !== "string"
      || !entry.sddl.includes("O:")
      || !entry.sddl.includes("D:")
    ) {
      throw new Error("Windows filesystem security snapshot contains an invalid descriptor");
    }
    const identity = entry.relativePath.replace(/\//g, "\\").toLocaleLowerCase("en-US");
    if (seen.has(identity)) {
      throw new Error("Windows filesystem security snapshot contains a duplicate path");
    }
    seen.add(identity);
    return Object.freeze({ ...entry });
  }));
}

/**
 * Capture every complete descriptor in a confined, non-reparse tree. The
 * snapshot is suitable for exact rollback after recursive role hardening.
 */
export async function snapshotWindowsFileSecurityTree(
  root: string,
  options: WindowsFilesystemSecurityOptions = {},
): Promise<WindowsFileSecurityTreeSnapshot> {
  const result = await runWindowsHelper<{ entries: WindowsFileSecurityTreeEntry[] }>(
    "snapshot-file-security-tree",
    [root],
    { helperPath: options.helperPath },
  );
  return Object.freeze({
    root,
    entries: checkedWindowsFileSecurityTreeEntries(result.entries),
  });
}

/**
 * Restore every descriptor through confined, non-following handles and require
 * the native helper to prove an exact owner/control/ACE round-trip.
 */
export async function restoreWindowsFileSecurityTree(
  snapshot: WindowsFileSecurityTreeSnapshot,
  options: WindowsFilesystemSecurityOptions = {},
): Promise<void> {
  const entries = checkedWindowsFileSecurityTreeEntries(snapshot.entries);
  const input = new TextEncoder().encode(JSON.stringify({ entries }));
  const result = await runWindowsHelper<{ restored: true; entries: number }>(
    "restore-file-security-tree",
    [snapshot.root],
    { helperPath: options.helperPath, input, sensitive: true },
  );
  if (
    result.restored !== true
    || !Number.isSafeInteger(result.entries)
    || result.entries !== entries.length
  ) {
    throw new Error("Windows filesystem security tree restore returned an invalid proof");
  }
}

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
    "ROOST_SERVICE_ACCOUNT_PASSWORD",
    "ROOST_BOOTSTRAP_TOKEN",
    "ROOST_SHAWL_PATH",
    "ROOST_WIN_HELPER",
    "ROOST_INSTALL_ROOT",
    "ROOST_STABLE_SHAWL_PATH",
    "ROOST_STABLE_LAUNCHER",
    "ROOST_SYSTEM32",
  ]);
  return Object.fromEntries(
    Object.entries(env).filter(([key]) => key.startsWith("ROOST_") && !blocked.has(key)),
  );
}

interface WindowsInstallContext {
  definitions: Readonly<Record<RoostServiceRole, WindowsServiceDefinition>>;
  credentials: WindowsServiceCredentials;
  installRoot: string;
  serviceDir: string;
  versionsRoot: string;
  activeHelperPath: string;
  interactiveSid: string;
}

function windowsDefinitions(options: {
  execPath: string;
  coordinatorHost: boolean;
  env: Record<string, string>;
  credentials?: WindowsServiceCredentials;
  coordinatorEnvironment?: Record<string, string>;
  workerEnvironment?: Record<string, string>;
}): WindowsInstallContext {
  const credentials = windowsCredentials(options.env, options.credentials);
  const releaseDir = dirname(options.execPath);
  const serviceDir = options.env.ROOST_SERVICE_DIR ?? roostServiceDir(undefined, "win32");
  const installRoot = options.env.ROOST_INSTALL_ROOT?.trim() || dirname(serviceDir);
  const versionsRoot = options.env.ROOST_VERSIONS_DIR ?? dirname(releaseDir);
  const stableBin = join(installRoot, "bin");
  const activeHelperPath = options.env.ROOST_WIN_HELPER ?? join(releaseDir, "roost-win-helper.exe");
  const publisher = requireWindowsValue(options.env, "ROOST_WINDOWS_PUBLISHER_SHA256");
  const serviceHome = join(serviceDir, "home");
  const serviceLocalAppData = join(serviceHome, "AppData", "Local");
  const interactiveSid = requireWindowsValue(options.env, "ROOST_INTERACTIVE_SID");
  if (!/^S-1-[0-9-]+$/.test(interactiveSid)) {
    throw new Error("ROOST_INTERACTIVE_SID must be a Windows SID");
  }
  if (!/^[0-9a-f]{64}$/i.test(publisher)) {
    throw new Error("ROOST_WINDOWS_PUBLISHER_SHA256 must be the pinned 64-hex signing leaf SHA-256");
  }
  const definitions = buildWindowsServiceDefinitions({
    executablePath: options.execPath,
    shawlPath: options.env.ROOST_STABLE_SHAWL_PATH ?? options.env.ROOST_SHAWL_PATH ?? join(stableBin, "shawl.exe"),
    serviceLauncherPath: options.env.ROOST_STABLE_LAUNCHER ?? join(stableBin, "roost.exe"),
    windowsHelperPath: activeHelperPath,
    account: credentials.account,
    coordinatorHost: options.coordinatorHost,
    serviceDir,
    commonEnvironment: {
      ROOST_SERVICE_DIR: serviceDir,
      ROOST_SYSTEM32: requireWindowsValue(options.env, "ROOST_SYSTEM32"),
      ROOST_VERSIONS_DIR: versionsRoot,
      ROOST_WINDOWS_PUBLISHER_SHA256: publisher.toUpperCase(),
      USERPROFILE: serviceHome,
      HOME: serviceHome,
      APPDATA: join(serviceHome, "AppData", "Roaming"),
      LOCALAPPDATA: serviceLocalAppData,
      TEMP: join(serviceLocalAppData, "Temp"),
      TMP: join(serviceLocalAppData, "Temp"),
      ROOST_INTERACTIVE_SID: interactiveSid,
    },
    roleEnvironment: {
      coordinator: options.coordinatorEnvironment ?? {},
      worker: options.workerEnvironment ?? {},
    },
  });
  return {
    definitions,
    credentials,
    installRoot,
    serviceDir,
    versionsRoot,
    activeHelperPath,
    interactiveSid,
  };
}

const WINDOWS_ROLE_STATE_PROFILE: Readonly<Record<RoostServiceRole, WindowsRoleStateProfile>> =
  Object.freeze({
    keeper: "keeper-state",
    worker: "worker-state",
    coordinator: "coordinator-state",
    updater: "updater-state",
  });

const WINDOWS_ROLE_DATA_ENV: Readonly<Record<RoostServiceRole, string | undefined>> =
  Object.freeze({
    keeper: "ROOST_WORKER_DATA_DIR",
    worker: "ROOST_WORKER_DATA_DIR",
    coordinator: "ROOST_COORD_DATA_DIR",
    updater: undefined,
  });

const WINDOWS_ROLE_LOG_ENV: Readonly<Record<RoostServiceRole, string | undefined>> =
  Object.freeze({
    keeper: "ROOST_WORKER_LOG_DIR",
    worker: "ROOST_WORKER_LOG_DIR",
    coordinator: "ROOST_COORD_LOG_DIR",
    updater: undefined,
  });

interface WindowsRoleStateRoot {
  path: string;
  profile: WindowsRoleStateProfile;
}

function windowsRoleStateRoots(
  definitions: Readonly<Record<RoostServiceRole, WindowsServiceDefinition>>,
): readonly WindowsRoleStateRoot[] {
  const byPath = new Map<string, WindowsRoleStateRoot>();
  const add = (path: string | undefined, profile: WindowsRoleStateProfile): void => {
    if (!path?.trim() || /[\0\r\n]/.test(path)) {
      throw new Error(`Windows ${profile} path is missing or invalid`);
    }
    const identity = win32.normalize(path).replace(/[\\/]+$/, "").toLocaleLowerCase("en-US");
    const existing = byPath.get(identity);
    if (existing && existing.profile !== profile) {
      throw new Error(`Windows role-state roots overlap across profiles: ${path}`);
    }
    if (!existing) byPath.set(identity, { path, profile });
  };
  for (const role of WINDOWS_SERVICE_ROLES) {
    const definition = definitions[role];
    const profile = WINDOWS_ROLE_STATE_PROFILE[role];
    const dataKey = WINDOWS_ROLE_DATA_ENV[role];
    const logKey = WINDOWS_ROLE_LOG_ENV[role];
    add(definition.cwd, profile);
    if (dataKey) add(definition.environment[dataKey], profile);
    add(logKey ? definition.environment[logKey] : definition.logDir, profile);
  }
  return Object.freeze([...byPath.values()].map((entry) => Object.freeze(entry)));
}

function windowsInstallDirectoryPaths(context: WindowsInstallContext): readonly string[] {
  const serviceHome = context.definitions.worker.environment.USERPROFILE;
  if (!serviceHome) throw new Error("Windows service-home path is missing");
  const paths = new Set<string>([
    context.installRoot,
    join(context.installRoot, "bin"),
    context.versionsRoot,
    context.serviceDir,
    join(context.serviceDir, "requests"),
    join(context.serviceDir, "requests", "interactive-update"),
    serviceHome,
    ...windowsRoleStateRoots(context.definitions).map((entry) => entry.path),
  ]);
  for (const definition of Object.values(context.definitions)) {
    paths.add(definition.logDir);
    for (const key of ["USERPROFILE", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP"] as const) {
      const path = definition.environment[key];
      if (path) paths.add(path);
    }
  }
  return Object.freeze([...paths]);
}

function prepareWindowsDirectories(context: WindowsInstallContext): void {
  for (const path of windowsInstallDirectoryPaths(context)) {
    mkdirSync(path, { recursive: true });
  }
}

async function protectWindowsInstallDirectories(
  context: WindowsInstallContext,
): Promise<void> {
  const serviceHome = context.definitions.worker.environment.USERPROFILE;
  if (!serviceHome) throw new Error("Windows service-home path is missing");
  const profiles = [
    [context.installRoot, "install-root"],
    [join(context.installRoot, "bin"), "stable-bin"],
    [context.versionsRoot, "versions-root"],
    [context.serviceDir, "service-root"],
    [join(context.serviceDir, "requests"), "update-inbox"],
    [join(context.serviceDir, "requests", "interactive-update"), "local-update-inbox"],
    [serviceHome, "service-home"],
  ] as const;
  for (const [path, profile] of profiles) {
    const result = await runWindowsHelper<{ protected: true; profile: string }>(
      "protect-directory",
      [path, profile, context.credentials.account, context.interactiveSid],
      { helperPath: context.activeHelperPath },
    );
    if (result.protected !== true || result.profile !== profile) {
      throw new Error(`Windows ${profile} protection returned an invalid proof`);
    }
  }
  for (const state of windowsRoleStateRoots(context.definitions)) {
    await protectWindowsRoleStateTree(state.path, state.profile, {
      account: context.credentials.account,
      interactiveSid: context.interactiveSid,
      helperPath: context.activeHelperPath,
    });
  }
}
const WINDOWS_SERVICE_STOP_ORDER = [
  "worker",
  "coordinator",
  "updater",
  "keeper",
] as const satisfies readonly RoostServiceRole[];

const WINDOWS_SERVICE_START_ORDER = [
  "keeper",
  "coordinator",
  "updater",
  "worker",
] as const satisfies readonly RoostServiceRole[];

type WindowsDesiredLifecycle = Readonly<
  Partial<Record<RoostServiceRole, "running" | "stopped">>
>;

interface WindowsDefinitionsFileBaseline {
  path: string;
  contents: Uint8Array | null;
}

function assertWindowsInstallBaseline(
  baseline: WindowsServiceSnapshotSet,
  expectedAccount: string,
): void {
  for (const role of WINDOWS_SERVICE_ROLES) {
    const service = baseline[role];
    if (!service.installed) continue;
    if (service.state !== "running" && service.state !== "stopped") {
      throw new Error(
        `${service.name} must reach a stable running/stopped state before security cutover`,
      );
    }
    if (
      service.account === null
      || normalizedWindowsAccount(service.account) !== normalizedWindowsAccount(expectedAccount)
    ) {
      throw new Error(
        `${service.name} account migration cannot be rolled back without its prior credential`,
      );
    }
    if (
      typeof service.securityDescriptor !== "string"
      || service.securityDescriptor.trim() === ""
      || service.serviceSidType === null
      || service.serviceSidType === undefined
    ) {
      throw new Error(`${service.name} full security snapshot is incomplete`);
    }
  }
}

function isWindowsPathWithin(parent: string, candidate: string): boolean {
  const relative = win32.relative(parent, candidate);
  return relative === ""
    || (!win32.isAbsolute(relative) && relative !== ".." && !relative.startsWith(`..${win32.sep}`));
}

function windowsFilesystemSnapshotRoots(context: WindowsInstallContext): readonly string[] {
  const serviceHome = context.definitions.worker.environment.USERPROFILE;
  if (!serviceHome) throw new Error("Windows service-home path is missing");
  const candidates = [
    context.installRoot,
    context.serviceDir,
    context.versionsRoot,
    join(context.installRoot, "bin"),
    join(context.serviceDir, "requests"),
    serviceHome,
    ...windowsRoleStateRoots(context.definitions).map((entry) => entry.path),
  ]
    .filter((path) => existsSync(path))
    .map((path) => win32.resolve(path))
    .sort((left, right) => left.length - right.length);
  const roots: string[] = [];
  for (const candidate of candidates) {
    if (!roots.some((root) => isWindowsPathWithin(root, candidate))) roots.push(candidate);
  }
  if (roots.length === 0) {
    throw new Error("Windows security cutover has no existing trusted filesystem root");
  }
  return Object.freeze(roots);
}

async function snapshotWindowsInstallFilesystem(
  context: WindowsInstallContext,
): Promise<WindowsFileSecurityTreeSnapshot[]> {
  const snapshots: WindowsFileSecurityTreeSnapshot[] = [];
  for (const root of windowsFilesystemSnapshotRoots(context)) {
    snapshots.push(await snapshotWindowsFileSecurityTree(root, {
      helperPath: context.activeHelperPath,
    }));
  }
  return snapshots;
}

function snapshotWindowsDefinitionsFile(context: WindowsInstallContext): WindowsDefinitionsFileBaseline {
  const path = windowsServiceDefinitionsPath({ ROOST_SERVICE_DIR: context.serviceDir });
  return {
    path,
    contents: existsSync(path) ? new Uint8Array(readFileSync(path)) : null,
  };
}

async function restoreWindowsDefinitionsFile(baseline: WindowsDefinitionsFileBaseline): Promise<void> {
  if (baseline.contents === null) {
    rmSync(baseline.path, { force: true });
    return;
  }
  // Rollback rewrites the trust anchor every Windows service definition is
  // read from; a torn write here must stay as durable as the forward write.
  await durableWriteFile(baseline.path, baseline.contents, { mode: 0o600, privateDacl: true });
}

async function stopWindowsServicesForCutover(manager: WindowsServiceManager): Promise<void> {
  for (const role of WINDOWS_SERVICE_STOP_ORDER) {
    const service = await manager.query(role);
    if (service.installed && service.state !== "stopped") await manager.stop(role);
  }
}

async function protectWindowsWorkerEnrollmentTree(context: WindowsInstallContext): Promise<void> {
  for (const state of windowsRoleStateRoots(context.definitions)) {
    if (state.profile !== "worker-state") continue;
    await protectWindowsRoleStateTree(state.path, state.profile, {
      account: context.credentials.account,
      interactiveSid: context.interactiveSid,
      helperPath: context.activeHelperPath,
    });
  }
}

function wantedWindowsServiceState(
  role: RoostServiceRole,
  baseline: WindowsServiceSnapshotSet,
  desired: WindowsDesiredLifecycle,
): "running" | "stopped" {
  return desired[role] ?? (baseline[role].state === "running" ? "running" : "stopped");
}

async function startWindowsServicesAfterCutover(
  manager: WindowsServiceManager,
  baseline: WindowsServiceSnapshotSet,
  desired: WindowsDesiredLifecycle,
): Promise<void> {
  for (const role of WINDOWS_SERVICE_START_ORDER) {
    if (wantedWindowsServiceState(role, baseline, desired) === "running") {
      await manager.start(role);
    }
  }
}

function removeWindowsCutoverDirectories(
  context: WindowsInstallContext,
  createdDirectories: readonly string[],
): void {
  const trustedRoots = [
    context.installRoot,
    context.serviceDir,
    context.versionsRoot,
    ...windowsRoleStateRoots(context.definitions).map((entry) => entry.path),
  ].map((path) => win32.resolve(path));
  const deepestFirst = [...createdDirectories]
    .map((path) => win32.resolve(path))
    .sort((left, right) => right.split(/[\\/]/).length - left.split(/[\\/]/).length);
  for (const path of deepestFirst) {
    if (!trustedRoots.some((root) => isWindowsPathWithin(root, path))) {
      throw new Error(`refusing to remove an unconfined Windows rollback path: ${path}`);
    }
    rmSync(path, { recursive: true, force: true });
  }
}

async function proveWindowsSecurityCutover(
  manager: WindowsServiceManager,
  baseline: WindowsServiceSnapshotSet,
  desired: WindowsDesiredLifecycle,
): Promise<void> {
  const current = await manager.snapshot({ includeSecurity: true });
  for (const role of WINDOWS_SERVICE_ROLES) {
    const service = current[role];
    const wanted = wantedWindowsServiceState(role, baseline, desired);
    if (
      !service.installed
      || service.serviceSidType !== "unrestricted"
      || typeof service.securityDescriptor !== "string"
      || service.securityDescriptor.trim() === ""
      || service.state !== wanted
    ) {
      throw new Error(`${service.name} did not prove the committed SID/ACL/lifecycle state`);
    }
  }
}

async function installWindowsServiceTopology(options: {
  context: WindowsInstallContext;
  desired: WindowsDesiredLifecycle;
  enrollWorker?: () => Promise<void>;
}): Promise<void> {
  const { context } = options;
  const manager = createWindowsServiceManager();
  const baseline = await manager.snapshot({ includeSecurity: true });
  assertWindowsInstallBaseline(baseline, context.credentials.account);
  const definitionsBaseline = snapshotWindowsDefinitionsFile(context);
  const createdDirectories = windowsInstallDirectoryPaths(context)
    .filter((path) => !existsSync(path));
  const filesystemBaseline: WindowsFileSecurityTreeSnapshot[] = [];
  let definitionsWriteAttempted = false;
  let directoriesPrepared = false;
  try {
    // Configure every inert next-launch definition and unrestricted service SID
    // before creating downtime. Existing services retain their current account
    // secret so rollback never depends on an unavailable previous password.
    for (const role of WINDOWS_SERVICE_ROLES) {
      await manager.install(
        context.definitions[role],
        baseline[role].installed ? undefined : context.credentials,
      );
    }

    // Every old process token must be gone before exact role ACLs replace the
    // shared-account layout. Stop dependents before Keeper.
    await stopWindowsServicesForCutover(manager);
    filesystemBaseline.push(...await snapshotWindowsInstallFilesystem(context));
    directoriesPrepared = true;
    prepareWindowsDirectories(context);

    await manager.provisionServiceSecurity(context.interactiveSid);
    await protectWindowsInstallDirectories(context);
    const currentPath = join(context.serviceDir, "current.json");
    if (!existsSync(currentPath)) {
      throw new Error("Windows current manifest is missing before security cutover");
    }
    await windowsProtectUpdaterArtifact(currentPath, "current", {
      helperPath: context.activeHelperPath,
    });

    if (options.enrollWorker) {
      await options.enrollWorker();
      // Enrollment creates the worker key as the elevated installer. Reapply
      // the role profile so no base-account or installer ACE survives.
      await protectWindowsWorkerEnrollmentTree(context);
    }

    await startWindowsServicesAfterCutover(manager, baseline, options.desired);
    await proveWindowsSecurityCutover(manager, baseline, options.desired);

    // Consumers may trust this file only after SCM, service SIDs, filesystem
    // ACLs, enrollment, and lifecycle have all committed and round-tripped.
    definitionsWriteAttempted = true;
    await storeWindowsServiceDefinitions(context.definitions);
  } catch (error) {
    const rollbackErrors: unknown[] = [];
    try {
      await stopWindowsServicesForCutover(manager);
    } catch (rollbackError) {
      rollbackErrors.push(rollbackError);
    }
    if (definitionsWriteAttempted) {
      try {
        await restoreWindowsDefinitionsFile(definitionsBaseline);
      } catch (rollbackError) {
        rollbackErrors.push(rollbackError);
      }
    }
    for (const snapshot of filesystemBaseline.toReversed()) {
      try {
        await restoreWindowsFileSecurityTree(snapshot, {
          helperPath: context.activeHelperPath,
        });
      } catch (rollbackError) {
        rollbackErrors.push(rollbackError);
      }
    }
    if (directoriesPrepared) {
      try {
        removeWindowsCutoverDirectories(context, createdDirectories);
      } catch (rollbackError) {
        rollbackErrors.push(rollbackError);
      }
    }
    try {
      await manager.restore(baseline, {
        restoreLifecycleRoles: WINDOWS_SERVICE_ROLES,
        allowKeeperStop: true,
      });
    } catch (rollbackError) {
      rollbackErrors.push(rollbackError);
    }
    if (rollbackErrors.length > 0) {
      throw new AggregateError(
        [error, ...rollbackErrors],
        "Windows service security cutover failed and exact rollback was incomplete",
      );
    }
    throw error;
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
        writeWindowsDryRun(context.definitions, WINDOWS_SERVICE_ROLES, opts.log);
        return;
      }
      await installWindowsServiceTopology({
        context,
        desired: { coordinator: "running" },
      });
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
        ...publicRoleEnvironment(env),
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
      const enrollWorker = opts.bootstrapToken
        ? async (): Promise<void> => {
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
        : undefined;
      await installWindowsServiceTopology({
        context,
        desired: {
          keeper: "running",
          worker: "running",
          coordinator: opts.coordinatorHost ? "running" : "stopped",
        },
        enrollWorker,
      });
      return;
    }
    default:
      throw new Error(`unsupported service platform: ${process.platform}`);
  }
}
