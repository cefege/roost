// Worker-side journal for coordinator relocations: durable, schema-versioned
// records of each handoff's decisions and broker commands, written through
// durableWriteFile so a crash mid-relocation leaves a readable trail the
// recovery pass can classify instead of guessing. Consumed by
// coord-relocation-recovery.ts and the Windows runtime.
import { randomUUID } from "node:crypto";
import * as fs from "node:fs";
import { dirname } from "node:path";
import { durableRemove, durableWriteFile } from "@roost/shared/durability";
import { log } from "@roost/shared/log";
import { WINDOWS_RELOCATION_SCHEMA_VERSION } from "@roost/shared/windows-relocation";
import type {
  WindowsRelocationBrokerCommand,
  WindowsRelocationCommandAction,
  WindowsRelocationResultFrame,
  WindowsWorkerEndpointRelocationOperation,
} from "@roost/shared/windows-relocation";
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
  windows_relocation?: WindowsWorkerEndpointRelocationOperation;
}

interface RelocationRequest {
  handoff_id: string;
  source_url: string;
  target_url: string;
}

/** Keeps the worker runtime and installed service endpoint coherent across a move. */
export class WorkerCoordRelocation {
  readonly #windowsRuntime: WindowsCoordRuntime;

  constructor(
    private readonly path: string,
    private readonly workerServicePath: string,
    windowsRuntime: WindowsCoordRuntime = createDefaultWindowsCoordRuntime(),
  ) {
    this.#windowsRuntime = windowsRuntime;
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

  /** Reconciles a durable Windows admission before network recovery starts. */
  async recoverTransaction(): Promise<void> {
    const current = this.load();
    switch (process.platform) {
      case "darwin":
      case "linux":
        return;
      case "win32": {
        if (current) {
          if (current.state === "COMMITTED") await this.#ensureWindowsAction(current, "COMMIT");
          else await this.#ensureWindowsAction(current, "START");
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
      // RoostUpdaterV2 owns the machine transaction and every privileged mutation.
      await this.#writeWindows({
        ...current,
        windows_endpoint_mutation: { url: current.target_url, started_at_ms: Date.now() },
        updated_at_ms: Date.now(),
      });
      await this.#ensureWindowsAction(current, "APPLY");
      const committed = { ...current, state: "COMMITTED" as const, windows_endpoint_mutation: undefined, updated_at_ms: Date.now() };
      await this.#writeWindows(committed);
      await this.#ensureWindowsAction(committed, "COMMIT");
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
        await this.#ensureWindowsAction(current, "RESTORE");
        await durableRemove(this.path, { platform: "win32", privateDacl: true });
        break;
      default:
        throw new Error(`unsupported worker relocation platform: ${process.platform}`);
    }
    relocate(current.source_url);
  }

  async #stageWindows(request: RelocationRequest): Promise<void> {
    const [service, expectedBefore] = await Promise.all([
      this.#windowsRuntime.queryService("worker"),
      this.#windowsRuntime.queryRelocationService("worker"),
    ]);
    if (!service.installed || service.state !== "running") {
      throw new Error("RoostWorkerV2 must be installed and running for relocation");
    }
    const operation: WindowsWorkerEndpointRelocationOperation = {
      schemaVersion: WINDOWS_RELOCATION_SCHEMA_VERSION,
      kind: "worker-endpoint",
      relocationId: randomUUID(),
      handoffId: request.handoff_id,
      sourceUrl: request.source_url,
      targetUrl: request.target_url,
      expectedBefore,
    };
    const journal = {
      version: 1 as const,
      ...request,
      state: "STAGED" as const,
      updated_at_ms: Date.now(),
      windows_service: service,
      windows_relocation: operation,
    };
    await this.#writeWindows(journal);
    await this.#ensureWindowsAction(journal, "START");
  }

  async #activateWindows(request: RelocationRequest, current: CoordRelocationJournal | null): Promise<void> {
    if (!current?.windows_relocation || !current.windows_service) {
      throw new Error("Windows relocation must be prepared before activation");
    }
    await this.#writeWindows({ ...current, ...request, state: "ACTIVATED", updated_at_ms: Date.now() });
  }

  async #discardWindows(_current: CoordRelocationJournal | null): Promise<void> {
    await durableRemove(this.path, { platform: "win32", privateDacl: true });
  }

  #requireActivated(): CoordRelocationJournal {
    const current = this.load();
    if (!current || current.state !== "ACTIVATED") throw new Error("worker relocation was not activated");
    return current;
  }

  async #ensureWindowsAction(
    current: CoordRelocationJournal,
    action: Exclude<WindowsRelocationCommandAction, "STATUS">,
  ): Promise<void> {
    const status = await this.#runWindowsCommand(current, "STATUS");
    if (action === "START" && status.phase === "missing") {
      await this.#runWindowsCommand(current, "START");
      return;
    }
    if (action === "START" && ["prepared", "applied", "committed"].includes(status.phase)) return;
    if (action === "APPLY" && status.phase === "prepared") {
      await this.#runWindowsCommand(current, "APPLY");
      return;
    }
    if (action === "APPLY" && ["applied", "committed"].includes(status.phase)) return;
    if (action === "COMMIT" && status.phase === "applied") {
      await this.#runWindowsCommand(current, "COMMIT");
      return;
    }
    if (action === "COMMIT" && status.phase === "committed") return;
    if (action === "RESTORE" && ["prepared", "applied"].includes(status.phase)) {
      await this.#runWindowsCommand(current, "RESTORE");
      return;
    }
    if (action === "RESTORE" && status.phase === "rolled-back") return;
    throw new Error(`Windows worker relocation ${action} refused in durable phase ${status.phase}: ${status.error}`);
  }

  async #runWindowsCommand(
    current: CoordRelocationJournal,
    action: WindowsRelocationCommandAction,
  ): Promise<WindowsRelocationResultFrame> {
    const operation = current.windows_relocation;
    if (!operation) throw new Error("Windows relocation journal has no updater operation");
    const command: WindowsRelocationBrokerCommand = {
      schemaVersion: WINDOWS_RELOCATION_SCHEMA_VERSION,
      requestId: randomUUID(),
      relocationId: operation.relocationId,
      handoffId: operation.handoffId,
      operationKind: operation.kind,
      action,
      operation: action === "START" ? operation : undefined,
    };
    const result = await this.#windowsRuntime.runRelocationCommand(command, async () => {});
    if (action !== "STATUS" && (!result.terminal || !result.success)) {
      throw new Error(result.error || `Windows relocation ${action} has no successful terminal result`);
    }
    return result;
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
    const escapedUrl = url
      .replaceAll("\\", "\\\\")
      .replaceAll("\"", "\\\"")
      .replaceAll("\n", "\\n")
      .replaceAll("\r", "\\r")
      .replaceAll("\t", "\\t");
    const line = `Environment="ROOST_COORDINATOR_URL=${escapedUrl}"`;
    const priorEndpoint = /^Environment=(?:"ROOST_COORDINATOR_URL=(?:\\.|[^"])*"|ROOST_COORDINATOR_URL=.*)$/gm;
    const withoutPriorEndpoint = original.replace(priorEndpoint, "");
    const rewritten = withoutPriorEndpoint.replace(/^\[Service\]$/m, `[Service]\n${line}`);
    if (rewritten === withoutPriorEndpoint || !rewritten.includes(line)) {
      throw new Error("failed to update worker coordinator URL");
    }
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
