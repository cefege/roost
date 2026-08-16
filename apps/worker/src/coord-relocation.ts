import * as fs from "node:fs";
import { dirname } from "node:path";
import { durableRemove, durableWriteFile } from "@roost/shared/durability";
import { log } from "@roost/shared/log";
import { machineRelocationTransaction, type MachineRelocationTransaction } from "./coord-relocation-transaction.ts";
import {
  createDefaultWindowsCoordRuntime,
  type WindowsCoordRuntime,
  type WindowsServiceSnapshot,
} from "./coord-relocation-windows-runtime.ts";

export interface CoordRelocationJournal {
  version: 1;
  handoff_id: string;
  source_url: string;
  target_url: string;
  state: "STAGED" | "ACTIVATED" | "COMMITTED";
  updated_at_ms: number;
  windows_service?: WindowsServiceSnapshot;
  windows_endpoint_mutation?: { url: string; started_at_ms: number };
}

interface RelocationRequest {
  handoff_id: string;
  source_url: string;
  target_url: string;
}

/** Keeps the worker runtime and installed service endpoint coherent across a move. */
export class WorkerCoordRelocation {
  readonly #windowsRuntime: WindowsCoordRuntime;
  readonly #transaction: MachineRelocationTransaction;

  constructor(
    private readonly path: string,
    private readonly workerServicePath: string,
    windowsRuntime: WindowsCoordRuntime = createDefaultWindowsCoordRuntime(),
    transaction: MachineRelocationTransaction = machineRelocationTransaction,
  ) {
    this.#windowsRuntime = windowsRuntime;
    this.#transaction = transaction;
  }

  load(): CoordRelocationJournal | null {
    try {
      const parsed = JSON.parse(fs.readFileSync(this.path, "utf8")) as CoordRelocationJournal;
      return parsed.version === 1 ? parsed : null;
    } catch {
      return null;
    }
  }

  stage(request: RelocationRequest): void | Promise<void> {
    switch (process.platform) {
      case "darwin":
      case "linux":
        this.#writePosix({ version: 1, ...request, state: "STAGED", updated_at_ms: Date.now() });
        return;
      case "win32":
        return this.#stageWindows(request);
      default:
        throw new Error(`unsupported worker relocation platform: ${process.platform}`);
    }
  }

  activate(request: RelocationRequest): void | Promise<void> {
    const current = this.load();
    if (current && current.handoff_id !== request.handoff_id && current.state !== "COMMITTED") {
      throw new Error(`worker already has an active relocation for handoff ${current.handoff_id}`);
    }
    switch (process.platform) {
      case "darwin":
      case "linux":
        this.#writePosix({ version: 1, ...request, state: "ACTIVATED", updated_at_ms: Date.now() });
        return;
      case "win32":
        return this.#activateWindows(request, current);
      default:
        throw new Error(`unsupported worker relocation platform: ${process.platform}`);
    }
  }

  /** Drops a journal that can no longer influence anything. */
  discard(): void | Promise<void> {
    const current = this.load();
    switch (process.platform) {
      case "darwin":
      case "linux":
        fs.rmSync(this.path, { force: true });
        this.#fsyncParent();
        return;
      case "win32":
        return this.#discardWindows(current);
      default:
        throw new Error(`unsupported worker relocation platform: ${process.platform}`);
    }
  }

  /** Reacquires the machine lock before network recovery/update handling starts. */
  async recoverTransaction(): Promise<void> {
    switch (process.platform) {
      case "darwin":
      case "linux":
        return;
      case "win32": {
        const current = this.load();
        if (current && current.state !== "COMMITTED") {
          await this.#transaction.acquire("win32", current.handoff_id, this.path, "worker-endpoint");
        }
        return;
      }
      default:
        throw new Error(`unsupported worker relocation platform: ${process.platform}`);
    }
  }

  async commit(unackedEventCount: () => number, relocate: (url: string, force?: boolean) => void): Promise<void> {
    const current = this.#requireActivated();
    const deadline = Date.now() + 30_000;
    while (unackedEventCount() > 0 && Date.now() < deadline) await Bun.sleep(25);
    const residual = unackedEventCount();
    if (residual > 0) throw new Error(`worker event drain timed out after 30000ms with ${residual} unacked events`);
    if (process.platform === "win32") {
      await this.#transaction.acquire("win32", current.handoff_id, this.path, "worker-endpoint");
      await this.#persistWindowsEndpoint(current, current.target_url);
      await this.#writeWindows({ ...current, state: "COMMITTED", windows_endpoint_mutation: undefined, updated_at_ms: Date.now() });
      await this.#transaction.release("win32", current.handoff_id, "worker-endpoint");
    } else if (process.platform === "darwin" || process.platform === "linux") {
      await this.#persistPosixEndpoint(current.target_url);
      this.#writePosix({ ...current, state: "COMMITTED", updated_at_ms: Date.now() });
    } else {
      throw new Error(`unsupported worker relocation platform: ${process.platform}`);
    }
    setTimeout(() => relocate(current.target_url, true), 0);
  }

  async abort(handoffId: string, relocate: (url: string) => void): Promise<void> {
    const current = this.load();
    if (!current || current.state === "COMMITTED" || current.handoff_id !== handoffId) return;
    switch (process.platform) {
      case "darwin":
      case "linux":
        await this.#persistPosixEndpoint(current.source_url);
        fs.rmSync(this.path, { force: true });
        this.#fsyncParent();
        break;
      case "win32":
        await this.#transaction.acquire("win32", current.handoff_id, this.path, "worker-endpoint");
        if (!current.windows_service) throw new Error("Windows relocation journal has no worker service snapshot");
        await this.#writeWindows({ ...current, windows_endpoint_mutation: { url: current.source_url, started_at_ms: Date.now() }, updated_at_ms: Date.now() });
        await this.#windowsRuntime.configureService(current.windows_service);
        await this.#assertWindowsService(current.windows_service);
        await durableRemove(this.path, { platform: "win32", privateDacl: true });
        await this.#transaction.release("win32", current.handoff_id, "worker-endpoint");
        break;
      default:
        throw new Error(`unsupported worker relocation platform: ${process.platform}`);
    }
    relocate(current.source_url);
  }

  async #stageWindows(request: RelocationRequest): Promise<void> {
    await this.#transaction.acquire("win32", request.handoff_id, this.path, "worker-endpoint");
    try {
      const service = await this.#windowsRuntime.queryService("worker");
      if (!service.installed || service.state !== "running") throw new Error("RoostWorkerV2 must be installed and running for relocation");
      await this.#writeWindows({ version: 1, ...request, state: "STAGED", updated_at_ms: Date.now(), windows_service: service });
    } catch (error) {
      await this.#transaction.release("win32", request.handoff_id, "worker-endpoint");
      throw error;
    }
  }

  async #activateWindows(request: RelocationRequest, current: CoordRelocationJournal | null): Promise<void> {
    await this.#transaction.acquire("win32", request.handoff_id, this.path, "worker-endpoint");
    try {
      const service = current?.windows_service ?? await this.#windowsRuntime.queryService("worker");
      await this.#writeWindows({ version: 1, ...request, state: "ACTIVATED", updated_at_ms: Date.now(), windows_service: service });
    } catch (error) {
      await this.#transaction.release("win32", request.handoff_id, "worker-endpoint");
      throw error;
    }
  }

  async #discardWindows(current: CoordRelocationJournal | null): Promise<void> {
    await durableRemove(this.path, { platform: "win32", privateDacl: true });
    if (current && current.state !== "COMMITTED") {
      await this.#transaction.release("win32", current.handoff_id, "worker-endpoint");
    }
  }

  #requireActivated(): CoordRelocationJournal {
    const current = this.load();
    if (!current || current.state !== "ACTIVATED") throw new Error("worker relocation was not activated");
    return current;
  }

  async #persistWindowsEndpoint(current: CoordRelocationJournal, url: string): Promise<void> {
    if (!current.windows_service) throw new Error("Windows relocation journal has no worker service snapshot");
    const before = await this.#windowsRuntime.queryService("worker");
    const desired: WindowsServiceSnapshot = {
      ...before,
      environment: { ...before.environment, ROOST_COORDINATOR_URL: url },
    };
    await this.#writeWindows({ ...current, windows_endpoint_mutation: { url, started_at_ms: Date.now() }, updated_at_ms: Date.now() });
    try {
      await this.#windowsRuntime.configureService(desired);
      await this.#assertWindowsService(desired);
    } catch (error) {
      await this.#windowsRuntime.configureService(current.windows_service).catch(() => {});
      throw error;
    }
  }

  async #assertWindowsService(expected: WindowsServiceSnapshot): Promise<void> {
    const actual = await this.#windowsRuntime.queryService("worker");
    const projection = (service: WindowsServiceSnapshot): string => JSON.stringify({
      ...service,
      state: undefined,
      dependencies: [...service.dependencies].sort(),
      environment: Object.fromEntries(Object.entries(service.environment).sort(([left], [right]) => left.localeCompare(right))),
    });
    if (projection(actual) !== projection(expected) || actual.state !== expected.state) {
      throw new Error("RoostWorkerV2 coordinator endpoint configuration did not round-trip through SCM");
    }
  }

  async #persistPosixEndpoint(url: string): Promise<void> {
    if (!fs.existsSync(this.workerServicePath)) throw new Error(`worker service definition is missing at ${this.workerServicePath}`);
    if (process.platform === "darwin") {
      const set = Bun.spawn(["/usr/libexec/PlistBuddy", "-c", `Set :EnvironmentVariables:ROOST_COORDINATOR_URL ${url}`, this.workerServicePath], { stdout: "ignore", stderr: "ignore" });
      if (await set.exited !== 0) throw new Error("failed to update worker coordinator URL");
      const get = Bun.spawn(["/usr/libexec/PlistBuddy", "-c", "Print :EnvironmentVariables:ROOST_COORDINATOR_URL", this.workerServicePath], { stdout: "pipe", stderr: "ignore" });
      const actual = (await new Response(get.stdout).text()).trim();
      if (await get.exited !== 0 || actual !== url) throw new Error("worker coordinator URL verification failed");
      return;
    }
    if (process.platform !== "linux") throw new Error(`unsupported worker relocation platform: ${process.platform}`);
    const original = fs.readFileSync(this.workerServicePath, "utf8");
    const line = `Environment=ROOST_COORDINATOR_URL=${url}`;
    const rewritten = /^Environment=ROOST_COORDINATOR_URL=.*$/m.test(original)
      ? original.replace(/^Environment=ROOST_COORDINATOR_URL=.*$/m, line)
      : original.replace(/^\[Service\]$/m, `[Service]\n${line}`);
    if (!rewritten.includes(line)) throw new Error("failed to update worker coordinator URL");
    const temporary = `${this.workerServicePath}.tmp-${process.pid}`;
    fs.writeFileSync(temporary, rewritten, { mode: 0o644 });
    fs.renameSync(temporary, this.workerServicePath);
    const reload = Bun.spawn(["systemctl", "--user", "daemon-reload"], {
      stdout: "ignore", stderr: "ignore",
      env: { ...process.env, XDG_RUNTIME_DIR: process.env.XDG_RUNTIME_DIR ?? `/run/user/${process.getuid?.() ?? ""}` },
    });
    if (await reload.exited !== 0) log.warn("worker", "systemd_daemon_reload_failed", { path: this.workerServicePath });
    if (!fs.readFileSync(this.workerServicePath, "utf8").includes(line)) throw new Error("worker coordinator URL verification failed");
  }

  #writePosix(value: CoordRelocationJournal): void {
    fs.mkdirSync(dirname(this.path), { recursive: true, mode: 0o700 });
    const temporary = `${this.path}.tmp-${process.pid}`;
    const fd = fs.openSync(temporary, "w", 0o600);
    try {
      fs.writeFileSync(fd, JSON.stringify(value));
      fs.fsyncSync(fd);
    } finally { fs.closeSync(fd); }
    fs.renameSync(temporary, this.path);
    this.#fsyncParent();
  }

  async #writeWindows(value: CoordRelocationJournal): Promise<void> {
    await durableWriteFile(this.path, `${JSON.stringify(value)}\n`, { platform: "win32", mode: 0o600, privateDacl: true });
  }

  #fsyncParent(): void {
    const fd = fs.openSync(dirname(this.path), "r");
    try { fs.fsyncSync(fd); } finally { fs.closeSync(fd); }
  }
}
