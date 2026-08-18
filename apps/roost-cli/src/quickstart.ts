// `roost quickstart` — the local one-shot installer. Single machine, zero agent.
// Turns the old 7-step manual install into one command: Tailscale gate →
// build SPA → mint TLS cert → install coord → deploy local worker → health →
// status → open the browser already-authorized via a self-minted #pair token.
//
// Tailscale is REQUIRED (the install stops without it — see resolveTailscale
// gate below). Other machines/workers are NOT set up here (deferred).
// Calls: deploy() for the local worker, statusReport() for the readout.

import { spawn } from "bun";
import { Database } from "bun:sqlite";
import {
  chmodSync,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  renameSync,
  rmdirSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { randomUUID } from "node:crypto";
import { homedir, tmpdir, userInfo } from "node:os";
import { basename, dirname, join, win32 } from "node:path";
import { deploy } from "./deploy.ts";
import { resolveTailscale, ensureTailscale, statusReport, printStatusReport } from "./status.ts";
import { ROOST_VERSION } from "./version.ts";
import {
  installCoordAgent,
  installWorkerAgent,
  protectWindowsRoleStateTree,
  readWindowsServiceCredentials,
  restoreWindowsFileSecurityTree,
  snapshotWindowsFileSecurityTree,
  type WindowsFileSecurityTreeSnapshot,
} from "./install-binary-agents.ts";
import { coordDataDir, roostServiceDir } from "@roost/shared/paths";
import { ROOST_BUILD_SHA } from "@roost/shared/build-identity";
import { probeServiceHealth } from "@roost/shared/service-health";
import {
  acquireMachineTransaction,
  type MachineTransactionLock,
} from "./machine-transaction.ts";
import {
  durableRemove,
  durableReplace,
  durableWriteFile,
  flushDurablePath,
} from "@roost/shared/durability";
import { windowsApplyArtifactDacl } from "@roost/shared/windows-helper";
import {
  WINDOWS_SERVICE_ROLES,
  createWindowsServiceManager,
  type WindowsServiceManager,
  type WindowsServiceSnapshotSet,
} from "./service-ctl.ts";

const WEB_DIST_INDEX = "apps/web/dist/index.html";

function logStep(msg: string): void {
  console.log(`>> ${msg}`);
}

function die(msg: string, ...hint: string[]): never {
  const error = new Error([msg, ...hint].join(" ")) as Error & { exitCode: number };
  error.exitCode = 1;
  throw error;
}

interface CoordinatorPaths {
  dataDir: string;
  logDir: string;
  tlsDir: string;
  database: string;
  authorizedKeys: string;
  key: string;
  handoff: string;
}

function requireCanonicalWindowsPath(name: string, expected?: string): string {
  const value = process.env[name]?.trim();
  if (!value || !win32.isAbsolute(value) || /[\0\r\n]/.test(value)) {
    throw new Error(`${name} must be an explicit absolute Windows path`);
  }
  const normalized = win32.normalize(value);
  const canonical = normalized.length > 3 ? normalized.replace(/[\\/]+$/, "") : normalized;
  if (expected) {
    const normalizedExpected = win32.normalize(expected);
    const canonicalExpected = normalizedExpected.length > 3
      ? normalizedExpected.replace(/[\\/]+$/, "")
      : normalizedExpected;
    if (canonical.toLocaleLowerCase("en-US") !== canonicalExpected.toLocaleLowerCase("en-US")) {
      throw new Error(`${name} does not match the canonical Windows install layout`);
    }
  }
  return canonical;
}


function coordinatorPaths(): CoordinatorPaths {
  if (process.platform !== "win32") {
    const dataDir = coordDataDir();
    return {
      dataDir,
      logDir: process.env.ROOST_COORD_LOG_DIR ?? join(dataDir, "..", "..", "logs", "coordinator"),
      tlsDir: process.env.ROOST_COORDINATOR_TLS_DIR ?? join(dataDir, "tls"),
      database: process.env.ROOST_COORDINATOR_DB ?? join(dataDir, "coordinator_v2.db"),
      authorizedKeys: process.env.ROOST_COORDINATOR_AUTHORIZED_KEYS ?? join(dataDir, "authorized_keys.roost"),
      key: process.env.ROOST_COORDINATOR_KEY_PATH ?? join(dataDir, "ssh_ed25519.key"),
      handoff: process.env.ROOST_COORDINATOR_HANDOFF_PATH ?? join(dataDir, "coord-handoff.json"),
    };
  }
  const installRoot = requireCanonicalWindowsPath("ROOST_INSTALL_ROOT");
  const serviceDir = requireCanonicalWindowsPath(
    "ROOST_SERVICE_DIR",
    win32.join(installRoot, "service"),
  );
  const dataDir = requireCanonicalWindowsPath(
    "ROOST_COORD_DATA_DIR",
    win32.join(serviceDir, "data", "coordinator"),
  );
  return {
    dataDir,
    logDir: requireCanonicalWindowsPath(
      "ROOST_COORD_LOG_DIR",
      win32.join(serviceDir, "logs", "coordinator"),
    ),
    tlsDir: requireCanonicalWindowsPath(
      "ROOST_COORDINATOR_TLS_DIR",
      win32.join(dataDir, "tls"),
    ),
    database: requireCanonicalWindowsPath(
      "ROOST_COORDINATOR_DB",
      win32.join(dataDir, "coordinator_v2.db"),
    ),
    authorizedKeys: requireCanonicalWindowsPath(
      "ROOST_COORDINATOR_AUTHORIZED_KEYS",
      win32.join(dataDir, "authorized_keys.roost"),
    ),
    key: requireCanonicalWindowsPath(
      "ROOST_COORDINATOR_KEY_PATH",
      win32.join(dataDir, "ssh_ed25519.key"),
    ),
    handoff: requireCanonicalWindowsPath(
      "ROOST_COORDINATOR_HANDOFF_PATH",
      win32.join(dataDir, "coord-handoff.json"),
    ),
  };
}

interface WindowsLegacyCoordinatorMigration {
  schemaVersion: 1;
  phase: "prepared" | "moved" | "hardened";
  legacyPath: string;
  canonicalPath: string;
  journalPath: string;
  helperPath: string;
  security: WindowsFileSecurityTreeSnapshot;
}

async function persistLegacyCoordinatorMigration(
  migration: WindowsLegacyCoordinatorMigration,
): Promise<void> {
  const temporary = `${migration.journalPath}.${process.pid}.${randomUUID()}.tmp`;
  try {
    await durableWriteFile(temporary, `${JSON.stringify(migration)}\n`, { platform: "win32" });
    await windowsApplyArtifactDacl(temporary, "NT SERVICE\\RoostUpdaterV2", {
      helperPath: migration.helperPath,
    });
    await durableReplace(temporary, migration.journalPath, { platform: "win32" });
    await windowsApplyArtifactDacl(migration.journalPath, "NT SERVICE\\RoostUpdaterV2", {
      helperPath: migration.helperPath,
    });
    await flushDurablePath(dirname(migration.journalPath), { platform: "win32" });
  } finally {
    await durableRemove(temporary, { platform: "win32" }).catch(() => undefined);
  }
}

async function removeLegacyMigrationAdditions(
  migration: WindowsLegacyCoordinatorMigration,
): Promise<void> {
  const current = await snapshotWindowsFileSecurityTree(migration.legacyPath, {
    helperPath: migration.helperPath,
  });
  const original = new Set(migration.security.entries.map((entry) =>
    entry.relativePath.replace(/\//g, "\\").toLocaleLowerCase("en-US")
  ));
  const additions = current.entries
    .filter((entry) =>
      entry.relativePath !== ""
      && !original.has(entry.relativePath.replace(/\//g, "\\").toLocaleLowerCase("en-US"))
    )
    .sort((left, right) =>
      right.relativePath.split(/[\\/]/).length - left.relativePath.split(/[\\/]/).length
    );
  for (const entry of additions) {
    const path = win32.join(migration.legacyPath, entry.relativePath);
    if (entry.kind === "file") unlinkSync(path);
    else rmdirSync(path);
  }
}

async function rollbackLegacyCoordinatorMigration(
  migration: WindowsLegacyCoordinatorMigration,
): Promise<void> {
  if (existsSync(migration.canonicalPath)) {
    if (existsSync(migration.legacyPath)) {
      throw new Error("cannot roll back legacy coordinator state because both roots exist");
    }
    renameSync(migration.canonicalPath, migration.legacyPath);
  }
  if (!existsSync(migration.legacyPath)) {
    throw new Error("cannot roll back legacy coordinator state because both roots are missing");
  }
  await removeLegacyMigrationAdditions(migration);
  await restoreWindowsFileSecurityTree(
    { ...migration.security, root: migration.legacyPath },
    { helperPath: migration.helperPath },
  );
  await durableRemove(migration.journalPath, { platform: "win32" });
  await flushDurablePath(dirname(migration.journalPath), { platform: "win32" });
}

async function prepareWindowsCoordinatorState(
  paths: CoordinatorPaths,
  account: string,
  journalPath: string,
): Promise<WindowsLegacyCoordinatorMigration | null> {
  const legacyPath = requireCanonicalWindowsPath("ROOST_LEGACY_COORD_DATA_DIR");
  if (existsSync(journalPath)) {
    const journalInfo = lstatSync(journalPath);
    if (!journalInfo.isFile() || journalInfo.isSymbolicLink()) {
      throw new Error("legacy coordinator migration journal is not a regular non-reparse file");
    }
    throw new Error(
      `incomplete legacy coordinator migration journal requires recovery before SCM mutation: ${journalPath}`,
    );
  }
  const helperPath = requireCanonicalWindowsPath("ROOST_WIN_HELPER");
  const interactiveSid = process.env.ROOST_INTERACTIVE_SID?.trim() ?? "";
  if (!/^S-1-(?:\d+-)+\d+$/.test(interactiveSid)) {
    throw new Error("ROOST_INTERACTIVE_SID is required for coordinator state migration");
  }
  if (legacyPath.toLocaleLowerCase("en-US") === paths.dataDir.toLocaleLowerCase("en-US")) {
    throw new Error("legacy and canonical coordinator data roots must differ");
  }
  mkdirSync(dirname(paths.dataDir), { recursive: true });
  if (!existsSync(legacyPath)) {
    if (!existsSync(paths.dataDir)) {
      mkdirSync(paths.dataDir, { recursive: false });
    }
    await protectWindowsRoleStateTree(paths.dataDir, "coordinator-state", {
      account,
      interactiveSid,
      helperPath,
    });
    return null;
  }
  if (existsSync(paths.dataDir)) {
    throw new Error("refusing to merge legacy and canonical coordinator data roots");
  }
  const security = await snapshotWindowsFileSecurityTree(legacyPath, { helperPath });
  let migration: WindowsLegacyCoordinatorMigration = {
    schemaVersion: 1,
    phase: "prepared",
    legacyPath,
    canonicalPath: paths.dataDir,
    journalPath,
    helperPath,
    security,
  };
  await persistLegacyCoordinatorMigration(migration);
  try {
    renameSync(legacyPath, paths.dataDir);
    migration = { ...migration, phase: "moved" };
    await persistLegacyCoordinatorMigration(migration);
    await protectWindowsRoleStateTree(paths.dataDir, "coordinator-state", {
      account,
      interactiveSid,
      helperPath,
    });
    migration = { ...migration, phase: "hardened" };
    await persistLegacyCoordinatorMigration(migration);
    return migration;
  } catch (error) {
    try {
      await rollbackLegacyCoordinatorMigration(migration);
    } catch (rollbackError) {
      throw new Error(`${String(error)}; legacy coordinator migration rollback failed: ${String(rollbackError)}`);
    }
    throw error;
  }
}

async function commitLegacyCoordinatorMigration(
  migration: WindowsLegacyCoordinatorMigration | null,
): Promise<void> {
  if (!migration) return;
  await durableRemove(migration.journalPath, { platform: "win32" });
  await flushDurablePath(dirname(migration.journalPath), { platform: "win32" });
}

/** Run a command with inherited stdio (user sees live output). Returns exit. */
async function runInherit(cmd: string[], cwd?: string, env?: Record<string, string>): Promise<number> {
  const proc = spawn({
    cmd,
    cwd,
    env: env ? { ...process.env, ...env } : undefined,
    stdio: ["inherit", "inherit", "inherit"],
  });
  await proc.exited;
  return proc.exitCode ?? 1;
}

/** From-source counterpart of install-binary-agents' dry-run isolation: force
 *  BOTH the darwin (*_PLIST) and linux (*_UNIT) knobs plus the data/log dirs
 *  into a throwaway dir, so `--dry-run` can never overwrite a real service
 *  definition or restart anything. Returns the path the current platform used. */
async function dryRunServiceDefinitions(coordUrl: string): Promise<void> {
  const darwin = process.platform === "darwin";
  const coordDir = mkdtempSync(join(tmpdir(), "roost-dryrun-coord-"));
  const coordEnv = {
    ROOST_COORD_LABEL: "com.roost.coordinator-dryrun",
    ROOST_COORD_PLIST: join(coordDir, "coord.plist"),
    ROOST_COORD_UNIT: join(coordDir, "coord.service"),
    ROOST_COORD_DATA_DIR: join(coordDir, "data"),
    ROOST_COORD_LOG_DIR: join(coordDir, "logs"),
  };
  logStep(`dry-run service definition → ${darwin ? coordEnv.ROOST_COORD_PLIST : coordEnv.ROOST_COORD_UNIT}`);
  if (await runInherit(["bash", "apps/coord/scripts/install.sh", "write-plist"], undefined, coordEnv) !== 0) {
    die("coord install.sh write-plist failed");
  }

  const workerDir = mkdtempSync(join(tmpdir(), "roost-dryrun-worker-"));
  const workerEnv = {
    ROOST_COORDINATOR_URL: coordUrl,
    ROOST_WORKER_AGENT_LABEL: "com.roost.worker-dryrun",
    ROOST_WORKER_PLIST: join(workerDir, "worker.plist"),
    ROOST_WORKER_UNIT: join(workerDir, "worker.service"),
    ROOST_WORKER_DATA_DIR: join(workerDir, "data"),
    ROOST_WORKER_LOG_DIR: join(workerDir, "logs"),
  };
  logStep(`dry-run service definition → ${darwin ? workerEnv.ROOST_WORKER_PLIST : workerEnv.ROOST_WORKER_UNIT}`);
  if (await runInherit(["bash", "apps/worker/scripts/install.sh", "write-plist"], undefined, workerEnv) !== 0) {
    die("worker install.sh write-plist failed");
  }
}

/** Run a command capturing output (for tailscale cert). */
function runCapture(cmd: string[]): { exit: number; stdout: string; stderr: string } {
  const r = Bun.spawnSync(cmd);
  return { exit: r.exitCode ?? 1, stdout: r.stdout.toString(), stderr: r.stderr.toString() };
}

/** Insert a one-shot browser bootstrap token directly into the coord DB —
 *  same row shape as router.ts::authMintBootstrap. No RPC, no loopback
 *  bypass: quickstart runs on the coord host and owns the DB file. The host
 *  browser redeems it via the public authRedeemBrowser on first load. */
function mintBrowserToken(label: string): string {
  const rand = new Uint8Array(18);
  crypto.getRandomValues(rand);
  const token = "roost_bt_" + Array.from(rand).map((b) => b.toString(16).padStart(2, "0")).join("");
  const now = Date.now();
  const db = new Database(coordinatorPaths().database);
  try {
    db.query(
      `INSERT INTO bootstrap_tokens (token, kind, label, created_at_ms, expires_at_ms, used_at_ms, used_by_fp)
       VALUES (?, 'browser', ?, ?, ?, NULL, NULL)`,
    ).run(token, label, now, now + 24 * 60 * 60 * 1000);
  } finally {
    db.close();
  }
  return token;
}

/** Insert a one-shot WORKER bootstrap token directly into the coord DB (kind
 *  "worker" — authRedeemWorker enrolls the worker's key from it). Same owner-
 *  of-the-DB-file basis as mintBrowserToken; used by binary-mode quickstart so
 *  the local worker authorizes on a fresh install without a manual token. */
function mintWorkerToken(label: string): string {
  const rand = new Uint8Array(18);
  crypto.getRandomValues(rand);
  const token = "roost_bt_" + Array.from(rand).map((b) => b.toString(16).padStart(2, "0")).join("");
  const now = Date.now();
  const db = new Database(coordinatorPaths().database);
  try {
    db.query(
      `INSERT INTO bootstrap_tokens (token, kind, label, created_at_ms, expires_at_ms, used_at_ms, used_by_fp)
       VALUES (?, 'worker', ?, ?, ?, NULL, NULL)`,
    ).run(token, label, now, now + 24 * 60 * 60 * 1000);
  } finally {
    db.close();
  }
  return token;
}

function trustedTailscaleExecutable(): string {
  if (process.platform !== "win32") return "tailscale";
  const executable = process.env.ROOST_TAILSCALE_EXE?.trim();
  if (!executable || !win32.isAbsolute(executable) || /[\0\r\n]/.test(executable)) {
    throw new Error("Windows quickstart requires the trusted absolute ROOST_TAILSCALE_EXE");
  }
  const info = lstatSync(executable);
  if (!info.isFile() || info.isSymbolicLink()) {
    throw new Error("ROOST_TAILSCALE_EXE must be a non-reparse regular file");
  }
  return executable;
}

/** Mint the tailnet TLS cert via `tailscale cert`. Skip if present unless force. */
function mintCert(fqdn: string, force: boolean): void {
  const tlsDir = coordinatorPaths().tlsDir;
  const certPath = join(tlsDir, `${fqdn}.crt`);
  const keyPath = join(tlsDir, `${fqdn}.key`);
  mkdirSync(tlsDir, { recursive: true });
  if (!force && existsSync(certPath) && existsSync(keyPath)) {
    logStep(`TLS cert present for ${fqdn} (skipping mint; --force to re-mint)`);
  } else {
    logStep(`minting TLS cert for ${fqdn}`);
    // On Linux `tailscale cert` writes through tailscaled and needs root or an
    // operator grant. The worker installer issues the same grant, but it runs
    // AFTER this — so take it here, best-effort, or a fresh box dies before the
    // thing that would have fixed it ever runs. `sudo -n` fails without cached
    // credentials; the cert call below is the real test either way.
    switch (process.platform) {
      case "linux":
        runCapture(["sudo", "-n", "tailscale", "set", `--operator=${userInfo().username}`]);
        break;
      case "darwin":
      case "win32":
        break;
      default:
        throw new Error(`unsupported quickstart platform: ${process.platform}`);
    }
    const cert = runCapture([
      trustedTailscaleExecutable(),
      "cert",
      "--cert-file",
      certPath,
      "--key-file",
      keyPath,
      fqdn,
    ]);
    if (cert.exit !== 0 && !existsSync(certPath)) {
      const detail = cert.stderr.trim() || cert.stdout.trim();
      if (process.platform !== "linux") {
        die(`tailscale cert failed: ${detail}`,
          "HTTPS is required (browsers need a secure context off localhost).");
      }
      die(`tailscale cert failed: ${detail}`,
        "HTTPS is required (browsers need a secure context off localhost).",
        "On Linux `tailscale cert` needs root or an operator grant. Run:",
        "  sudo tailscale set --operator=$USER",
        "then re-run `roost quickstart`.");
    }
  }
  console.log(`   cert: ${certPath}`);
}

/** Drop an `roost` shim on PATH so `roost status` / `roost logs` work from any
 *  directory (the workspace bin isn't globally linked). Targets ~/.bun/bin —
 *  the Bun installer the curl front-door uses puts it on PATH. Returns the
 *  shim path, or null if the dir isn't on PATH (caller prints a hint). */
function installRoostShim(repoDir: string): { path: string; onPath: boolean } | null {
  const binDir = join(homedir(), ".bun", "bin");
  try {
    mkdirSync(binDir, { recursive: true });
    const shim = join(binDir, "roost");
    writeFileSync(shim, `#!/usr/bin/env bash\nexec "${process.execPath}" "${join(repoDir, "apps/roost-cli/src/main.ts")}" "$@"\n`);
    chmodSync(shim, 0o755);
    const onPath = (process.env.PATH ?? "").split(":").includes(binDir);
    return { path: shim, onPath };
  } catch {
    return null;
  }
}

async function waitForCoordHealth(fqdn: string, timeoutMs = 15_000): Promise<boolean> {
  const url = `https://${fqdn}:4102/roost.v1.CoordinatorService/MiscHealth`;
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: "{}",
        signal: AbortSignal.timeout(3000),
      });
      if (res.ok) return true;
    } catch { /* not up yet */ }
    await new Promise((r) => setTimeout(r, 750));
  }
  return false;
}

interface WindowsInstallRollback {
  lock: MachineTransactionLock;
  manager: WindowsServiceManager;
  snapshot: WindowsServiceSnapshotSet;
  committed: boolean;
  migration: WindowsLegacyCoordinatorMigration | null;
}

async function proveWindowsInstallHealth(
  manager: WindowsServiceManager,
  expectedAccount: string,
  expectedCoordinatorUrl: string,
): Promise<void> {
  const snapshots = await manager.snapshot();
  for (const role of ["keeper", "worker", "coordinator"] as const) {
    const service = snapshots[role];
    if (!service.installed || service.state !== "running") {
      throw new Error(`${service.name} did not reach the required running state`);
    }
    if (service.account?.trim().toLowerCase() !== expectedAccount.trim().toLowerCase()) {
      throw new Error(`${service.name} does not use the dedicated service account`);
    }
  }
  const updater = snapshots.updater;
  if (
    !updater.installed
    || updater.startMode !== "automatic"
    || updater.account?.trim().toLowerCase() !== expectedAccount.trim().toLowerCase()
  ) {
    throw new Error("RoostUpdaterV2 automatic recovery/account proof failed");
  }
  await Promise.all([
    probeServiceHealth("coordinator", {
      expectedVersion: ROOST_VERSION,
      expectedBuild: ROOST_BUILD_SHA,
    }),
    probeServiceHealth("worker", {
      expectedVersion: ROOST_VERSION,
      expectedBuild: ROOST_BUILD_SHA,
      expectedCoordinatorUrl,
    }),
  ]);
  const launcher = process.env.ROOST_STABLE_LAUNCHER?.trim();
  if (!launcher || !win32.isAbsolute(launcher) || /[\0\r\n]/.test(launcher)) {
    throw new Error("Windows stable launcher path was not provided by the signed installer");
  }
  const launcherInfo = lstatSync(launcher);
  if (!launcherInfo.isFile() || launcherInfo.isSymbolicLink()) {
    throw new Error("Windows stable launcher is not a non-reparse regular file");
  }
  const version = Bun.spawnSync([launcher, "version"]);
  if (version.exitCode !== 0 || version.stdout.toString().trim() !== ROOST_VERSION) {
    throw new Error("Windows stable launcher did not execute the selected Roost version");
  }
  const stableBin = dirname(launcher).replace(/[\\/]+$/, "").toLowerCase();
  const pathSegments = (process.env.Path ?? process.env.PATH ?? "")
    .split(";")
    .map((segment) => segment.replace(/[\\/]+$/, "").toLowerCase());
  if (!pathSegments.includes(stableBin)) {
    throw new Error("Windows stable launcher directory is not present on PATH");
  }
}

export async function quickstart(args: string[]): Promise<void> {
  const force = args.includes("--force");
  const dry = args.includes("--dry-run"); // generate service definitions only
  const binary = basename(process.execPath).toLocaleLowerCase("en-US") !== "bun"
    && basename(process.execPath).toLocaleLowerCase("en-US") !== "bun.exe";
  switch (process.platform) {
    case "darwin":
    case "linux":
    case "win32":
      break;
    default:
      throw new Error(`unsupported quickstart platform: ${process.platform}`);
  }
  if (process.platform === "win32" && !binary) {
    die("Windows quickstart requires the signed compiled release", "run install-binary.ps1");
  }
  const serviceCredentials = process.platform === "win32" && !dry
    ? args.includes("--windows-service-credential-stdin")
      ? await readWindowsServiceCredentials()
      : die(
        "Windows service credential frame is required",
        "run quickstart through the signed install-binary.ps1 front door",
      )
    : undefined;
  let windowsInstall: WindowsInstallRollback | null = null;
  let windowsPaths: CoordinatorPaths | null = null;
  try {
  windowsPaths = process.platform === "win32" ? coordinatorPaths() : null;
  if (process.platform === "win32" && binary && !dry && serviceCredentials && windowsPaths) {
    const serviceDir = roostServiceDir(undefined, "win32");
    const transactionJournalPath = join(serviceDir, "install-transaction.json");
    const migrationJournalPath = join(serviceDir, "coordinator-migration.json");
    const lock = await acquireMachineTransaction(
      "install",
      transactionJournalPath,
      { platform: "win32" },
    );
    const manager = createWindowsServiceManager();
    try {
      windowsInstall = {
        lock,
        manager,
        snapshot: await manager.snapshot({ includeSecurity: true }),
        committed: false,
        migration: null,
      };
    } catch (error) {
      await lock.release();
      throw error;
    }
    windowsInstall.migration = await prepareWindowsCoordinatorState(
      windowsPaths,
      serviceCredentials.account,
      migrationJournalPath,
    );
  }
  // 1. Tailscale gate (interactive; skipped for --dry-run).
  let fqdn: string;
  if (dry) {
    fqdn = resolveTailscale().fqdn ?? "dry-run.example.ts.net";
    logStep(`--dry-run (plist generation only), tailnet ${fqdn}`);
  } else {
    logStep("checking Tailscale");
    try {
      const ready = await ensureTailscale({
        resolve: resolveTailscale,
        log: (m) => console.log(`   ${m}`),
        sleep: (ms) => Bun.sleep(ms),
        now: Date.now,
        brewInstall: async () => {
          if (Bun.which("brew")) await runInherit(["brew", "install", "tailscale"]);
        },
      });
      fqdn = ready.fqdn;
    } catch (e) {
      die(e instanceof Error ? e.message : String(e));
    }
    console.log(`   tailnet: ${fqdn}`);
  }
  const coordUrl = `https://${fqdn}:4102`;
  windowsPaths = process.platform === "win32" ? windowsPaths ?? coordinatorPaths() : null;
  const windowsCoordinatorEnvironment = windowsPaths
    ? {
      ROOST_COORD_DATA_DIR: windowsPaths.dataDir,
      ROOST_COORD_LOG_DIR: windowsPaths.logDir,
      ROOST_COORDINATOR_DB: windowsPaths.database,
      ROOST_COORDINATOR_AUTHORIZED_KEYS: windowsPaths.authorizedKeys,
      ROOST_COORDINATOR_KEY_PATH: windowsPaths.key,
      ROOST_COORDINATOR_HANDOFF_PATH: windowsPaths.handoff,
      ROOST_COORDINATOR_TLS_DIR: windowsPaths.tlsDir,
      ROOST_COORDINATOR_BIND: "0.0.0.0:4102",
      ROOST_COORDINATOR_PUBLIC_URL: coordUrl,
      ROOST_TLS_CERT_PATH: join(windowsPaths.tlsDir, `${fqdn}.crt`),
      ROOST_TLS_KEY_PATH: join(windowsPaths.tlsDir, `${fqdn}.key`),
    }
    : undefined;

  if (binary) {
    // Compiled binary: SPA + migrations embedded, no repo. Skip bun install +
    // vite build; install the coord/worker services that run `roost coord` /
    // `roost worker` via the embedded install scripts (reusing the
    // FRONTED/TLS/serve bash).
    console.log(`   roost: ${process.execPath} (${ROOST_VERSION})`);
    if (!dry) mintCert(fqdn, force);
    await installCoordAgent({
      execPath: process.execPath, gitSha: ROOST_VERSION,
      cmd: dry ? "write-plist" : "install", credentials: serviceCredentials,
      env: windowsCoordinatorEnvironment, log: logStep,
    });
    if (dry) {
      await installWorkerAgent({
        execPath: process.execPath, coordUrl, gitSha: ROOST_VERSION,
        cmd: "write-plist", coordinatorHost: true,
        coordinatorEnvironment: windowsCoordinatorEnvironment, log: logStep,
      });
      console.log(`\n✓ --dry-run complete (service definitions generated; nothing installed).`);
      return;
    }
    // Coord must be healthy (DB migrated) before we mint the worker's bootstrap
    // token into it; the worker redeems it on first boot to enroll its key.
    logStep("waiting for coordinator health");
    if (!await waitForCoordHealth(fqdn)) {
      die(`coord did not become healthy at ${coordUrl}`, "check logs: roost logs coord");
    }
    console.log(`   coord healthy at ${coordUrl}`);
    const workerToken = mintWorkerToken(`quickstart-${fqdn}`);
    await installWorkerAgent({
      execPath: process.execPath, coordUrl, bootstrapToken: workerToken,
      gitSha: ROOST_VERSION, cmd: "install", coordinatorHost: true,
      coordinatorEnvironment: windowsCoordinatorEnvironment,
      credentials: serviceCredentials, log: logStep,
    });
    if (windowsInstall && serviceCredentials) {
      await proveWindowsInstallHealth(
        windowsInstall.manager,
        serviceCredentials.account,
        coordUrl,
      );
    }
  } else {
    // From source: deps + build + the on-disk install scripts.
    console.log(`   bun: ${process.execPath}`);
    if (dry) {
      // --dry-run used to fall straight through this branch and do a REAL
      // install: it rewrote the live service definitions, restarted both
      // services and deployed a worker. Generate into a temp dir and stop.
      await dryRunServiceDefinitions(coordUrl);
      console.log(`\n✓ --dry-run complete (service definitions generated; nothing installed).`);
      return;
    }
    if (force || !existsSync("node_modules")) {
      logStep("bun install");
      if (await runInherit([process.execPath, "install"]) !== 0) die("bun install failed");
    } else {
      logStep("bun install (skipped — node_modules present; --force to reinstall)");
    }
    if (force || !existsSync(WEB_DIST_INDEX)) {
      logStep("building web SPA (apps/web → dist)");
      if (await runInherit([process.execPath, "x", "vite", "build"], "apps/web") !== 0) {
        die("vite build failed");
      }
    } else {
      logStep("web SPA build (skipped — dist present)");
    }
    mintCert(fqdn, force);
    logStep("installing coordinator service");
    if (await runInherit(["bash", "apps/coord/scripts/install.sh", "install"]) !== 0) {
      die("coord install.sh failed");
    }
    logStep("deploying local worker");
    process.env.ROOST_COORDINATOR_URL = coordUrl;
    process.env.ROOST_ALLOW_DIRTY = "1";
    await deploy(["localhost", "--allow-unpublished-local"]);
    logStep("waiting for coordinator health");
    if (!await waitForCoordHealth(fqdn)) {
      die(`coord did not become healthy at ${coordUrl}`, "check logs: roost logs coord");
    }
    console.log(`   coord healthy at ${coordUrl}`);
  }

  // Status readout.
  const report = await statusReport();
  printStatusReport(report);
  if (
    windowsInstall
    && (!report.tailscale.running
      || !report.coordAgentLoaded
      || !report.workerAgentLoaded
      || !report.coord.reachable)
  ) {
    throw new Error("Windows quickstart status proof did not confirm all required services");
  }

  // Authorize this machine's browser with no token paste: mint a #pair token +
  // open it. The fragment never reaches the coordinator or an HTTP Referer.
  logStep("opening the app (browser self-authorizes via #pair)");
  const token = mintBrowserToken(`quickstart-${fqdn}`);
  const openUrl = `${coordUrl}/#pair=${encodeURIComponent(token)}`;
  switch (process.platform) {
    case "linux":
      await runInherit(["xdg-open", openUrl]);
      break;
    case "darwin":
      await runInherit(["open", openUrl]);
      break;
    case "win32":
      await runInherit(["explorer.exe", openUrl]);
      break;
    default:
      throw new Error(`unsupported quickstart platform: ${process.platform}`);
  }

  const shim = binary ? null : installRoostShim(process.cwd());
  console.log(`\n✓ Roost is running.`);
  console.log(`  This machine:    ${openUrl}`);
  console.log(`  Pair your phone: open ${coordUrl} → Settings → Pair a device → scan the QR`);
  if (binary || (shim && shim.onPath)) {
    console.log(`  Health anytime:  roost status`);
  } else if (shim) {
    console.log(`  Health anytime:  ${shim.path} status   (add ~/.bun/bin to PATH for bare \`roost\`)`);
  } else {
    console.log(`  Health anytime:  bun apps/roost-cli/src/main.ts status`);
  }
  if (windowsInstall) {
    await commitLegacyCoordinatorMigration(windowsInstall.migration);
    windowsInstall.committed = true;
  }
  } catch (error) {
    if (windowsInstall && !windowsInstall.committed) {
      let rollbackError: unknown = null;
      try {
        if (windowsInstall.migration) {
          await windowsInstall.manager.stop("coordinator");
        }
        await windowsInstall.manager.restore(windowsInstall.snapshot, {
          restoreLifecycleRoles: windowsInstall.migration ? [] : WINDOWS_SERVICE_ROLES,
          allowKeeperStop: true,
        });
        if (windowsInstall.migration) {
          await rollbackLegacyCoordinatorMigration(windowsInstall.migration);
          windowsInstall.migration = null;
          await windowsInstall.manager.restore(windowsInstall.snapshot, {
            restoreLifecycleRoles: WINDOWS_SERVICE_ROLES,
            allowKeeperStop: true,
          });
        }
      } catch (caught) {
        rollbackError = caught;
      }
      if (rollbackError) {
        throw new Error(
          `${String(error)}; Windows install rollback failed: ${String(rollbackError)}`,
        );
      }
    }
    throw error;
  } finally {
    await windowsInstall?.lock.release();
    if (serviceCredentials) serviceCredentials.password = undefined;
  }
}
