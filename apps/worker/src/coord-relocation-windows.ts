// Builds the Windows half of a coordinator relocation as an explicit
// operation sequence (admit / apply / rollback / commit) against the service
// control manager via relocation broker commands. The POSIX path in
// coord-target.ts never runs here: SCM cutover must be transactional.
import { randomUUID } from "node:crypto";
import * as fs from "node:fs";
import { dirname, join } from "node:path";
import { roostServiceDir, roostVersionsDir } from "@roost/shared/paths";
import {
  WINDOWS_RELOCATION_SCHEMA_VERSION,
  type WindowsCoordinatorPromotionRelocationOperation,
  type WindowsRelocationBrokerCommand,
  type WindowsRelocationCommandAction,
  type WindowsRelocationResultFrame,
} from "@roost/shared/windows-relocation";
import {
  createDefaultWindowsCoordRuntime,
  type WindowsCoordRuntime,
} from "./coord-relocation-windows-runtime.ts";

export interface WindowsCoordinatorTargetPaths {
  dataDir: string;
  dbPath: string;
  keyPath: string;
  authorizedKeysPath: string;
  handoffPath: string;
  servicePath: string;
  currentManifestPath: string;
}

interface PersistedPreparedTarget {
  handoffId?: unknown;
  windowsRelocation?: unknown;
}

/**
 * Unprivileged Worker-side admission facade. It may inspect SCM and write the
 * handoff staging tree, but every authoritative file, ACL, route, override, and
 * lifecycle mutation is performed by RoostUpdaterV2 after durable admission.
 */
export class WindowsCoordinatorTargetRelocation {
  readonly #runtime: WindowsCoordRuntime;

  constructor(
    readonly paths: WindowsCoordinatorTargetPaths,
    runtime: WindowsCoordRuntime = createDefaultWindowsCoordRuntime(),
  ) {
    this.#runtime = runtime;
  }

  async createOperation(
    handoffId: string,
    sourceUrl: string,
    targetUrl: string,
    expectedGitSha: string,
  ): Promise<WindowsCoordinatorPromotionRelocationOperation> {
    const serviceDir = dirname(this.paths.currentManifestPath);
    return {
      schemaVersion: WINDOWS_RELOCATION_SCHEMA_VERSION,
      kind: "coordinator-promotion",
      relocationId: randomUUID(),
      handoffId,
      sourceUrl,
      targetUrl,
      expectedGitSha,
      expectedBefore: await this.#runtime.queryRelocationService("coordinator"),
      paths: {
        installRoot: dirname(serviceDir),
        serviceDir,
        versionsDir: roostVersionsDir(),
        serviceDefinitionsPath: this.paths.servicePath,
        coordinatorDataDir: this.paths.dataDir,
        coordinatorLogDir: join(serviceDir, "logs", "coordinator"),
        coordinatorDbPath: this.paths.dbPath,
        coordinatorKeyPath: this.paths.keyPath,
        coordinatorAuthorizedKeysPath: this.paths.authorizedKeysPath,
        coordinatorHandoffPath: this.paths.handoffPath,
      },
    };
  }

  /** Resume only the updater's durable transaction; never repairs state here. */
  async recoverActive(): Promise<void> {
    const handoffs = join(roostServiceDir(), "data", "worker", "relocation");
    if (!fs.existsSync(handoffs)) return;
    for (const entry of fs.readdirSync(handoffs, { withFileTypes: true })) {
      if (!entry.isDirectory()) continue;
      const operation = this.#loadOperation(join(handoffs, entry.name, "prepared.json"), entry.name);
      if (!operation) continue;
      const status = await this.#run(operation, "STATUS");
      if (status.phase !== "missing" && status.phase !== "rolled-back" && status.terminal && !status.success) {
        throw new Error(status.error || "Windows coordinator relocation recovery failed");
      }
    }
  }

  async admit(operation: WindowsCoordinatorPromotionRelocationOperation): Promise<void> {
    await this.#ensureAction(operation, "START");
  }

  async apply(operation: WindowsCoordinatorPromotionRelocationOperation): Promise<void> {
    await this.#ensureAction(operation, "START");
    await this.#ensureAction(operation, "APPLY");
  }

  async commit(operation: WindowsCoordinatorPromotionRelocationOperation): Promise<void> {
    await this.#ensureAction(operation, "COMMIT");
  }

  async rollback(operation: WindowsCoordinatorPromotionRelocationOperation): Promise<void> {
    await this.#ensureAction(operation, "RESTORE");
  }

  async #ensureAction(
    operation: WindowsCoordinatorPromotionRelocationOperation,
    action: Exclude<WindowsRelocationCommandAction, "STATUS">,
  ): Promise<void> {
    let status = await this.#run(operation, "STATUS");
    if (action === "START") {
      if (status.phase === "missing") status = await this.#run(operation, "START");
      else if (["prepared", "applied", "committed"].includes(status.phase)) return;
    } else if (action === "APPLY") {
      if (status.phase === "prepared") status = await this.#run(operation, "APPLY");
      else if (["applied", "committed"].includes(status.phase)) return;
    } else if (action === "COMMIT") {
      if (status.phase === "applied") status = await this.#run(operation, "COMMIT");
      else if (status.phase === "committed") return;
    } else {
      if (status.phase === "missing" || status.phase === "rolled-back") return;
      if (["prepared", "applied"].includes(status.phase)) status = await this.#run(operation, "RESTORE");
    }
    if (!status.terminal || !status.success) {
      throw new Error(status.error || `Windows coordinator relocation ${action} failed in phase ${status.phase}`);
    }
  }

  async #run(
    operation: WindowsCoordinatorPromotionRelocationOperation,
    action: WindowsRelocationCommandAction,
  ): Promise<WindowsRelocationResultFrame> {
    const command: WindowsRelocationBrokerCommand = {
      schemaVersion: WINDOWS_RELOCATION_SCHEMA_VERSION,
      requestId: randomUUID(),
      relocationId: operation.relocationId,
      handoffId: operation.handoffId,
      operationKind: operation.kind,
      action,
      operation: action === "START" ? operation : undefined,
    };
    return await this.#runtime.runRelocationCommand(command, async () => {});
  }

  #loadOperation(path: string, handoffId: string): WindowsCoordinatorPromotionRelocationOperation | null {
    try {
      const prepared = JSON.parse(fs.readFileSync(path, "utf8")) as PersistedPreparedTarget;
      const operation = prepared.windowsRelocation as WindowsCoordinatorPromotionRelocationOperation | undefined;
      if (prepared.handoffId !== handoffId || operation?.kind !== "coordinator-promotion" || operation.handoffId !== handoffId) {
        return null;
      }
      return operation;
    } catch {
      return null;
    }
  }
}
