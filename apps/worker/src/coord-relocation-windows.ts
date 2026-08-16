import * as fs from "node:fs";
import { randomUUID } from "node:crypto";
import { dirname, join, resolve } from "node:path";
import { machineRelocationTransaction, type MachineRelocationTransaction } from "./coord-relocation-transaction.ts";
import {
  WINDOWS_SERVICE_NAMES,
  createDefaultWindowsCoordRuntime,
  type WindowsCoordRuntime,
  type WindowsDaclSnapshot,
  type WindowsServiceRole,
  type WindowsServiceSnapshot,
} from "./coord-relocation-windows-runtime.ts";

const SERVICE_ROLES: readonly WindowsServiceRole[] = ["keeper", "worker", "coordinator", "updater"];
const MIN_WINDOWS_BUILD = 17_763;

export interface WindowsCoordinatorTargetPaths {
  dataDir: string;
  dbPath: string;
  keyPath: string;
  authorizedKeysPath: string;
  handoffPath: string;
  servicePath: string;
  currentManifestPath: string;
}

interface RollbackFile { canonical: string; backup: string; existed: boolean; backupDurable: boolean; dacl?: WindowsDaclSnapshot }
interface RollbackDirectory { path: string; dacl: WindowsDaclSnapshot }
interface MutationBoundary { sequence: number; atMs: number; operation: string; subject?: string }
interface WindowsCoordinatorJournal {
  schemaVersion: 1;
  handoffId: string;
  phase: "CAPTURED" | "PREPARED" | "PROMOTING" | "HEALTHY" | "ROLLING_BACK" | "ROLLBACK_FAILED" | "ROLLED_BACK" | "COMMITTED";
  targetUrl: string;
  priorHealthUrl: string;
  services: WindowsServiceSnapshot[];
  files: RollbackFile[];
  directories: RollbackDirectory[];
  tailscaleConfig: string;
  priorHealth: boolean;
  boundaries: MutationBoundary[];
  updatedAtMs: number;
  error?: string;
}

export interface WindowsPromotionFiles { db: string; key: string; authorizedKeys: string; handoff: string }
interface WindowsInspection { services: WindowsServiceSnapshot[]; directories: RollbackDirectory[] }

function normalizedAccount(account: string): string { return account.trim().replace(/^\.\\/, "").toLowerCase(); }
function normalizedPath(path: string): string { return resolve(path).replaceAll("\\", "/").replace(/\/$/, "").toLowerCase(); }
function existingDirectory(path: string): string {
  let candidate = path;
  while (!fs.existsSync(candidate)) {
    const parent = dirname(candidate);
    if (parent === candidate) throw new Error(`no existing directory for ${path}`);
    candidate = parent;
  }
  return fs.statSync(candidate).isDirectory() ? candidate : dirname(candidate);
}
function privateDacl(dacl: WindowsDaclSnapshot): boolean {
  return !/(?:;;;(?:WD|AU|BU)\)|;;;S-1-1-0\)|;;;S-1-5-11\)|;;;S-1-5-32-545\))/i.test(dacl.sddl);
}
function occurrences(argv: readonly string[], value: string): number { return argv.filter((argument) => argument === value).length; }
function valueAfter(argv: readonly string[], option: string): string | null {
  const index = argv.indexOf(option);
  return index >= 0 ? argv[index + 1] ?? null : null;
}
function sortedRecord(value: Record<string, string>): Record<string, string> {
  return Object.fromEntries(Object.entries(value).sort(([left], [right]) => left.localeCompare(right)));
}
function serviceConfiguration(snapshot: WindowsServiceSnapshot): unknown {
  return {
    role: snapshot.role, name: snapshot.name, installed: snapshot.installed,
    startMode: snapshot.startMode, imagePath: snapshot.imagePath, imageArgv: snapshot.imageArgv,
    account: snapshot.account, dependencies: [...snapshot.dependencies].sort(),
    environment: sortedRecord(snapshot.environment), shawlPath: snapshot.shawlPath,
    shawlArguments: snapshot.shawlArguments, executablePath: snapshot.executablePath,
    arguments: snapshot.arguments, cwd: snapshot.cwd, logDir: snapshot.logDir,
    displayName: snapshot.displayName, description: snapshot.description,
    recoveryPolicy: {
      ...snapshot.recoveryPolicy,
      actions: snapshot.recoveryPolicy.actions.map((action) => ({ ...action })),
    },
    securityDescriptor: snapshot.securityDescriptor,
  };
}
function serviceConfigMatches(left: WindowsServiceSnapshot, right: WindowsServiceSnapshot): boolean {
  return JSON.stringify(serviceConfiguration(left)) === JSON.stringify(serviceConfiguration(right));
}

export class WindowsCoordinatorTargetTransaction {
  readonly #runtime: WindowsCoordRuntime;
  readonly #transaction: MachineRelocationTransaction;
  constructor(
    readonly paths: WindowsCoordinatorTargetPaths,
    runtime: WindowsCoordRuntime = createDefaultWindowsCoordRuntime(),
    transaction: MachineRelocationTransaction = machineRelocationTransaction,
  ) {
    this.#runtime = runtime;
    this.#transaction = transaction;
  }

  journalPath(handoffId: string): string { return join(this.paths.dataDir, "handoffs", handoffId, "windows-relocation.json"); }

  async recoverActive(): Promise<void> {
    const handoffs = join(this.paths.dataDir, "handoffs");
    if (!fs.existsSync(handoffs)) return;
    const active: WindowsCoordinatorJournal[] = [];
    for (const entry of fs.readdirSync(handoffs, { withFileTypes: true })) {
      if (!entry.isDirectory()) continue;
      const journal = this.#loadJournal(entry.name);
      if (journal && journal.phase !== "COMMITTED" && journal.phase !== "ROLLED_BACK") active.push(journal);
    }
    if (active.length > 1) throw new Error("multiple active Windows coordinator relocation journals");
    const journal = active[0];
    if (!journal) return;
    await this.#transaction.acquire("win32", journal.handoffId, this.journalPath(journal.handoffId), "coordinator-target");
    if (journal.phase === "CAPTURED" || journal.phase === "PROMOTING"
      || journal.phase === "ROLLING_BACK" || journal.phase === "ROLLBACK_FAILED") {
      await this.rollback(journal.handoffId);
    }
  }

  async preflight(handoffId: string, allowActiveCoordinator: boolean): Promise<void> {
    const journalPath = this.journalPath(handoffId);
    await this.#transaction.acquire("win32", handoffId, journalPath, "coordinator-check");
    try { await this.#inspect(allowActiveCoordinator); }
    finally { await this.#transaction.release("win32", handoffId, "coordinator-check"); }
  }

  async prepare(handoffId: string, targetUrl: string, allowActiveCoordinator: boolean): Promise<void> {
    const journalPath = this.journalPath(handoffId);
    await this.#transaction.acquire("win32", handoffId, journalPath, "coordinator-target");
    const existing = this.#loadJournal(handoffId);
    if (existing) {
      if (existing.targetUrl !== targetUrl) throw new Error("prepared Windows relocation target URL changed");
      if (["PREPARED", "PROMOTING", "HEALTHY"].includes(existing.phase)) return;
      throw new Error(`Windows relocation is ${existing.phase.toLowerCase()}`);
    }
    let journal: WindowsCoordinatorJournal | null = null;
    try {
      const inspection = await this.#inspect(allowActiveCoordinator);
      const handoffDir = dirname(journalPath);
      const rollbackDir = join(handoffDir, "rollback", "windows");
      fs.mkdirSync(rollbackDir, { recursive: true, mode: 0o700 });
      const tailscaleConfig = await this.#runtime.captureTailscaleConfig(rollbackDir);
      const coordinator = inspection.services.find((service) => service.role === "coordinator")!;
      const priorHealthUrl = coordinator.environment.ROOST_COORDINATOR_PUBLIC_URL || targetUrl;
      const files = await this.#describeFiles(rollbackDir);
      journal = {
        schemaVersion: 1, handoffId, phase: "CAPTURED", targetUrl, priorHealthUrl,
        services: inspection.services, files, directories: inspection.directories, tailscaleConfig,
        priorHealth: coordinator.state === "running" && await this.#runtime.coordinatorHealthy(priorHealthUrl),
        boundaries: [], updatedAtMs: Date.now(),
      };
      await this.#writeJournal(journal);
      await this.#mutate(journal, "apply-private-dacl", handoffDir, () => this.#runtime.applyPrivateDacl(handoffDir));
      await this.#mutate(journal, "apply-private-dacl", rollbackDir, () => this.#runtime.applyPrivateDacl(rollbackDir));
      if (process.env.ROOST_FRONTED !== "0") {
        await this.#mutate(journal, "probe-tailscale-serve", targetUrl, () => this.#runtime.configureTailscaleServe());
        await this.#mutate(journal, "restore-tailscale-after-probe", undefined, () =>
          this.#runtime.restoreTailscaleConfig(tailscaleConfig, rollbackDir));
        await this.#proveHealth(journal.priorHealthUrl, journal.priorHealth);
      }
      if (coordinator.state === "running") {
        await this.#mutate(journal, "stop-service", WINDOWS_SERVICE_NAMES.coordinator, () => this.#runtime.stopService("coordinator", 15_000));
      }
      await this.#waitForDatabaseRelease();
      for (const file of files) {
        if (!file.existed) continue;
        await this.#boundary(journal, "capture-file", file.canonical);
        fs.copyFileSync(file.canonical, file.backup);
        await this.#runtime.flush(file.backup);
        await this.#runtime.applyPrivateDacl(file.backup);
        file.backupDurable = true;
        await this.#boundary(journal, "capture-file-complete", file.canonical);
      }
      journal.phase = "PREPARED";
      await this.#writeJournal(journal);
    } catch (error) {
      if (!journal) {
        await this.#transaction.release("win32", handoffId, "coordinator-target");
        throw error;
      }
      try { await this.rollback(handoffId); }
      catch (rollbackError) { throw new Error(`${(error as Error).message}; Windows target rollback failed: ${(rollbackError as Error).message}`); }
      throw error;
    }
  }

  async ensurePrepared(handoffId: string): Promise<void> {
    const journal = this.#requireJournal(handoffId);
    if (!["PREPARED", "PROMOTING", "HEALTHY"].includes(journal.phase)) throw new Error(`Windows relocation is not prepared (${journal.phase})`);
    await this.#transaction.acquire("win32", handoffId, this.journalPath(handoffId), "coordinator-target");
  }

  async beforeFileMutation(handoffId: string, operation: string, path: string): Promise<void> {
    await this.ensurePrepared(handoffId);
    await this.#boundary(this.#requireJournal(handoffId), operation, path);
  }

  async promote(handoffId: string, staged: WindowsPromotionFiles, expectedGitSha: string): Promise<void> {
    const journal = this.#requireJournal(handoffId);
    await this.ensurePrepared(handoffId);
    journal.phase = "PROMOTING";
    await this.#writeJournal(journal);
    try {
      for (const suffix of ["-wal", "-shm"]) {
        const path = `${this.paths.dbPath}${suffix}`;
        if (fs.existsSync(path)) await this.#mutate(journal, "remove-file", path, () => this.#runtime.durableRemove(path));
      }
      const replacements: readonly (readonly [string, string])[] = [
        [staged.db, this.paths.dbPath], [staged.key, this.paths.keyPath],
        [staged.authorizedKeys, this.paths.authorizedKeysPath], [staged.handoff, this.paths.handoffPath],
      ];
      for (const [source, destination] of replacements) {
        await this.#mutate(journal, "replace-file", destination, () => this.#runtime.durableReplace(source, destination));
        await this.#mutate(journal, "apply-private-dacl", destination, () => this.#runtime.applyPrivateDacl(destination));
      }
      const prior = journal.services.find((service) => service.role === "coordinator")!;
      const promoted: WindowsServiceSnapshot = {
        ...prior, state: "stopped", startMode: "automatic",
        environment: {
          ...prior.environment, ROOST_COORD_DATA_DIR: this.paths.dataDir,
          ROOST_COORDINATOR_DB: this.paths.dbPath, ROOST_COORDINATOR_KEY_PATH: this.paths.keyPath,
          ROOST_COORDINATOR_AUTHORIZED_KEYS: this.paths.authorizedKeysPath,
          ROOST_COORDINATOR_HANDOFF_PATH: this.paths.handoffPath,
          ROOST_COORDINATOR_PUBLIC_URL: journal.targetUrl, ROOST_GIT_SHA: expectedGitSha,
          ROOST_SKIP_ENV_LOCAL: "1", ROOST_LOG_ENCODING: "utf-8",
        },
      };
      await this.#mutate(journal, "configure-service", promoted.name, () => this.#runtime.configureService(promoted));
      if (!serviceConfigMatches(await this.#runtime.queryService("coordinator"), promoted)) throw new Error("RoostCoordinatorV2 configuration did not round-trip through SCM");
      if (process.env.ROOST_FRONTED !== "0") await this.#mutate(journal, "configure-tailscale", journal.targetUrl, () => this.#runtime.configureTailscaleServe());
      await this.#mutate(journal, "start-service", WINDOWS_SERVICE_NAMES.coordinator, () => this.#runtime.startService("coordinator"));
      await this.#proveHealth(journal.targetUrl, true);
      journal.phase = "HEALTHY";
      journal.error = undefined;
      await this.#writeJournal(journal);
    } catch (error) {
      try { await this.rollback(handoffId); }
      catch (rollbackError) { throw new Error(`${(error as Error).message}; Windows target rollback failed: ${(rollbackError as Error).message}`); }
      throw error;
    }
  }

  async rollback(handoffId: string): Promise<void> {
    const journal = this.#loadJournal(handoffId);
    if (!journal) return;
    await this.#transaction.acquire("win32", handoffId, this.journalPath(handoffId), "coordinator-target");
    if (journal.phase === "COMMITTED" || journal.phase === "ROLLED_BACK") {
      await this.#transaction.release("win32", handoffId, "coordinator-target");
      return;
    }
    journal.phase = "ROLLING_BACK";
    await this.#writeJournal(journal);
    const errors: string[] = [];
    const attempt = async (operation: () => Promise<void>): Promise<void> => {
      try { await operation(); } catch (error) { errors.push((error as Error).message); }
    };
    const active = await this.#runtime.queryService("coordinator").catch((error) => { errors.push(String(error)); return null; });
    if (active?.state === "running") await attempt(() => this.#mutate(journal, "rollback-stop-service", active.name, () => this.#runtime.stopService("coordinator", 15_000)));
    await attempt(() => this.#waitForDatabaseRelease());
    for (const file of journal.files) {
      await attempt(async () => {
        if (!file.existed) {
          if (fs.existsSync(file.canonical)) await this.#mutate(journal, "rollback-remove-file", file.canonical, () => this.#runtime.durableRemove(file.canonical));
          return;
        }
        if (!file.backupDurable) return;
        const restore = `${file.backup}.restore-${randomUUID()}`;
        await this.#boundary(journal, "rollback-restore-file", file.canonical);
        fs.copyFileSync(file.backup, restore);
        await this.#runtime.flush(restore);
        await this.#runtime.durableReplace(restore, file.canonical);
        if (file.dacl) await this.#runtime.applySddl(file.canonical, file.dacl.sddl);
      });
    }
    await attempt(() => this.#mutate(journal, "rollback-tailscale", undefined, () => this.#runtime.restoreTailscaleConfig(journal.tailscaleConfig, dirname(this.journalPath(handoffId)))));
    for (const prior of journal.services) await attempt(() => this.#mutate(journal, "rollback-configure-service", prior.name, () => this.#runtime.configureService(prior)));
    for (const prior of journal.services) {
      await attempt(async () => {
        const current = await this.#runtime.queryService(prior.role);
        if (prior.state === "running" && current.state !== "running") await this.#mutate(journal, "rollback-start-service", prior.name, () => this.#runtime.startService(prior.role));
        else if (prior.state === "stopped" && current.state !== "stopped") {
          if (prior.role === "keeper") throw new Error("refusing to stop RoostKeeperV2 during relocation rollback");
          await this.#mutate(journal, "rollback-stop-service", prior.name, () => this.#runtime.stopService(prior.role, 15_000));
        }
      });
    }
    for (const directory of journal.directories) await attempt(() => this.#mutate(journal, "rollback-dacl", directory.path, () => this.#runtime.applySddl(directory.path, directory.dacl.sddl)));
    await attempt(() => this.#proveHealth(journal.priorHealthUrl, journal.priorHealth));
    for (const prior of journal.services) {
      await attempt(async () => {
        const restored = await this.#runtime.queryService(prior.role);
        if (!serviceConfigMatches(restored, prior) || restored.state !== prior.state) throw new Error(`${prior.name} was not restored to its exact service configuration and state`);
      });
    }
    if (errors.length) {
      journal.phase = "ROLLBACK_FAILED";
      journal.error = errors.join("; ");
      await this.#writeJournal(journal);
      throw new Error(journal.error);
    }
    journal.phase = "ROLLED_BACK";
    journal.error = undefined;
    await this.#writeJournal(journal);
    await this.#transaction.release("win32", handoffId, "coordinator-target");
  }

  async commit(handoffId: string): Promise<void> {
    const journal = this.#loadJournal(handoffId);
    if (!journal) return;
    await this.#transaction.acquire("win32", handoffId, this.journalPath(handoffId), "coordinator-target");
    if (journal.phase !== "HEALTHY" && journal.phase !== "COMMITTED") throw new Error(`cannot commit Windows relocation in phase ${journal.phase}`);
    journal.phase = "COMMITTED";
    journal.error = undefined;
    await this.#writeJournal(journal);
    try {
      fs.rmSync(dirname(this.journalPath(handoffId)), { recursive: true, force: true });
      await this.#runtime.flush(join(this.paths.dataDir, "handoffs"));
    } finally { await this.#transaction.release("win32", handoffId, "coordinator-target"); }
  }

  async #inspect(allowActiveCoordinator: boolean): Promise<WindowsInspection> {
    if (this.#runtime.platform !== "win32") throw new Error(`Windows coordinator runtime reported ${this.#runtime.platform}`);
    if (this.#runtime.arch !== "x64") throw new Error(`Windows coordinator relocation requires x64, found ${this.#runtime.arch}`);
    const [major, minor, build] = this.#runtime.release.split(".").map((part) => Number.parseInt(part, 10));
    if (major !== 10 || minor !== 0 || !Number.isFinite(build) || build < MIN_WINDOWS_BUILD) throw new Error(`Windows 10 build ${MIN_WINDOWS_BUILD} or newer is required, found ${this.#runtime.release}`);
    await this.#runtime.tailscaleReady();
    const services = await Promise.all(SERVICE_ROLES.map((role) => this.#runtime.queryService(role)));
    this.#assertServiceTopology(services, allowActiveCoordinator);
    const protectedPaths = [
      ...(fs.existsSync(this.paths.dataDir) ? [this.paths.dataDir] : []),
      existingDirectory(this.paths.currentManifestPath), this.paths.currentManifestPath,
      ...(fs.existsSync(this.paths.servicePath) ? [this.paths.servicePath] : []),
    ];
    const directories: RollbackDirectory[] = [];
    for (const path of [...new Set(protectedPaths.map((candidate) => resolve(candidate)))]) {
      const dacl = await this.#runtime.readDacl(path);
      if (!privateDacl(dacl)) throw new Error(`Windows DACL is not private: ${path}`);
      if (fs.statSync(path).isDirectory()) directories.push({ path, dacl });
    }
    return { services, directories };
  }

  #assertServiceTopology(services: WindowsServiceSnapshot[], allowActiveCoordinator: boolean): void {
    const byRole = Object.fromEntries(services.map((service) => [service.role, service])) as Record<WindowsServiceRole, WindowsServiceSnapshot>;
    const operator = normalizedAccount(byRole.worker.account);
    if (!operator || /^(localsystem|nt authority\\system|localservice|networkservice|nt authority\\local service|nt authority\\network service)$/.test(operator)) throw new Error("Roost services must run as the chosen operator account, never a built-in service account");
    let manifest: { schemaVersion?: number; versionDir?: string };
    try { manifest = JSON.parse(fs.readFileSync(this.paths.currentManifestPath, "utf8")) as typeof manifest; }
    catch (error) { throw new Error(`invalid Windows current manifest: ${(error as Error).message}`); }
    if (manifest.schemaVersion !== 1 || !manifest.versionDir) throw new Error("invalid Windows current manifest schema");
    const expectedExecutable = normalizedPath(join(manifest.versionDir, "roost.exe"));
    for (const role of SERVICE_ROLES) {
      const service = byRole[role];
      if (!service || service.name !== WINDOWS_SERVICE_NAMES[role] || !service.installed) throw new Error(`Windows coordinator relocation requires configured service ${WINDOWS_SERVICE_NAMES[role]}`);
      if (service.state === "start-pending" || service.state === "stop-pending") throw new Error(`${service.name} has a pending SCM transition`);
      if (normalizedAccount(service.account) !== operator) throw new Error("all four Roost services must use the same operator account");
      if (normalizedPath(service.executablePath) !== expectedExecutable) throw new Error(`${service.name} is not configured for current version ${manifest.versionDir}`);
      const argv = service.shawlArguments;
      if (occurrences(argv, "--no-restart") !== 1 || occurrences(argv, "--kill-process-tree") !== 1 || occurrences(argv, "--stop-timeout") !== 1 || valueAfter(argv, "--stop-timeout") !== "15000" || valueAfter(argv, "--cwd") !== service.cwd || valueAfter(argv, "--log-dir") !== service.logDir || valueAfter(argv, "--log-rotate") !== "bytes=2097152" || valueAfter(argv, "--log-retain") !== "2" || service.environment.ROOST_LOG_ENCODING?.toLowerCase() !== "utf-8") throw new Error(`${service.name} does not have the required Shawl durability and log configuration`);
    }
    if (byRole.keeper.startMode !== "automatic" || byRole.keeper.state !== "running") throw new Error("RoostKeeperV2 must be automatic and running");
    if (byRole.worker.startMode !== "automatic" || byRole.worker.state !== "running" || !byRole.worker.dependencies.includes(WINDOWS_SERVICE_NAMES.keeper)) throw new Error("RoostWorkerV2 must be automatic, running, and depend on RoostKeeperV2");
    if (byRole.updater.startMode !== "manual" || byRole.updater.state !== "stopped") throw new Error("RoostUpdaterV2 must be demand-start and stopped outside an update transaction");
    const coordinator = byRole.coordinator;
    if (allowActiveCoordinator) {
      if (coordinator.startMode !== "automatic" || coordinator.state !== "running") throw new Error("retired RoostCoordinatorV2 must retain its automatic/running service state");
    } else if (coordinator.startMode !== "manual" || coordinator.state !== "stopped") throw new Error("worker-only hosts require a dormant/manual RoostCoordinatorV2 service");
  }

  async #describeFiles(rollbackDir: string): Promise<RollbackFile[]> {
    const candidates = [this.paths.dbPath, `${this.paths.dbPath}-wal`, `${this.paths.dbPath}-shm`, this.paths.keyPath, this.paths.authorizedKeysPath, this.paths.handoffPath, this.paths.servicePath, this.paths.currentManifestPath];
    const files: RollbackFile[] = [];
    for (const [index, canonical] of [...new Set(candidates.map((path) => resolve(path)))].entries()) {
      const existed = fs.existsSync(canonical);
      files.push({ canonical, backup: join(rollbackDir, `${index}-${canonical.replaceAll(/[^A-Za-z0-9_.-]/g, "_")}`), existed, backupDurable: false, dacl: existed ? await this.#runtime.readDacl(canonical) : undefined });
    }
    return files;
  }

  async #waitForDatabaseRelease(): Promise<void> {
    const paths = [this.paths.dbPath, `${this.paths.dbPath}-wal`, `${this.paths.dbPath}-shm`];
    const deadline = Date.now() + 15_000;
    for (;;) {
      if ((await Promise.all(paths.map((path) => this.#runtime.probeExclusiveOpen(path)))).every(Boolean)) return;
      if (Date.now() >= deadline) throw new Error("coordinator database/WAL handles were not released after SCM STOPPED");
      await Bun.sleep(100);
    }
  }

  async #proveHealth(targetUrl: string, expected: boolean): Promise<void> {
    for (let attempt = 0; attempt < 8; attempt++) {
      if (attempt > 0) await Bun.sleep(1_000);
      if (await this.#runtime.coordinatorHealthy(targetUrl) === expected) return;
    }
    throw new Error(expected ? `RoostCoordinatorV2 did not become healthy at ${targetUrl}` : `rollback did not restore the prior unhealthy state at ${targetUrl}`);
  }

  #loadJournal(handoffId: string): WindowsCoordinatorJournal | null {
    try {
      const journal = JSON.parse(fs.readFileSync(this.journalPath(handoffId), "utf8")) as WindowsCoordinatorJournal;
      if (journal.schemaVersion !== 1 || journal.handoffId !== handoffId) throw new Error("identity mismatch");
      return journal;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return null;
      throw new Error(`invalid Windows coordinator relocation journal: ${(error as Error).message}`);
    }
  }
  #requireJournal(handoffId: string): WindowsCoordinatorJournal {
    const journal = this.#loadJournal(handoffId);
    if (!journal) throw new Error("Windows coordinator target was not prepared");
    return journal;
  }
  async #writeJournal(journal: WindowsCoordinatorJournal): Promise<void> {
    journal.updatedAtMs = Date.now();
    await this.#runtime.durableWrite(this.journalPath(journal.handoffId), `${JSON.stringify(journal)}\n`, 0o600);
  }
  async #boundary(journal: WindowsCoordinatorJournal, operation: string, subject?: string): Promise<void> {
    journal.boundaries.push({ sequence: journal.boundaries.length + 1, atMs: Date.now(), operation, subject });
    await this.#writeJournal(journal);
  }
  async #mutate(journal: WindowsCoordinatorJournal, operation: string, subject: string | undefined, mutation: () => Promise<void>): Promise<void> {
    await this.#boundary(journal, operation, subject);
    await mutation();
  }
}
