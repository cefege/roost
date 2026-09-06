// Owns the isolated SQLite and fake worker-transport seam for global-search tests.
// Focused suites use it to seed dashboard-scoped sessions, capture exact batches,
// and settle real coordinator pending RPCs without opening network listeners.
// Every installed worker handle is removed during reset and cleanup.

import {
  createContextValues,
  type HandlerContext,
  type ServiceImpl,
} from "@connectrpc/connect";
import { CoordinatorService } from "@roost/shared/proto/coordinator_pb";
import type {
  CoordWorkerDown,
  DBrowserCommand,
} from "@roost/shared/proto/worker_transport_pb";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import {
  callerKey,
  dashboardActorKey,
  tabIdKey,
} from "../src/connect/auth-interceptor.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-service.ts";
import { resolvePendingRpc } from "../src/router/pending-rpcs.ts";
import { makeSessionGlobalSearchHandlers } from "../src/connect/handlers-sessions-global-search.ts";
import { GlobalSearchCursorOwner } from "../src/connect/global-search-cursors.ts";
import { GlobalSearchWorkerLaneOwner } from "../src/connect/global-search-worker-lanes.ts";
import type { ConnectDeps } from "../src/connect/router.ts";

export const GLOBAL_TEST_DASHBOARD_A = "global-search-dashboard-a";
export const GLOBAL_TEST_DASHBOARD_B = "global-search-dashboard-b";
export const GLOBAL_TEST_WORKER_A1 = "a".repeat(64);
export const GLOBAL_TEST_WORKER_A2 = "b".repeat(64);
export const GLOBAL_TEST_WORKER_B = "c".repeat(64);

export interface CapturedGlobalSearchCommand {
  workerFp: string;
  browserCommand: DBrowserCommand;
  control: Record<string, unknown>;
}

export class GlobalSearchTestWorker {
  readonly commands: CapturedGlobalSearchCommand[] = [];
  readonly #waiters = new Set<() => void>();
  #throwOnSend = false;

  constructor(
    readonly workerFp: string,
    readonly dashboardId: string,
  ) {}

  install(): void {
    __setConnectWorkerForTest(this.workerFp, {
      workerFp: this.workerFp,
      dashboardId: this.dashboardId,
      send: (frame) => this.#capture(frame),
    });
  }

  remove(): void {
    __setConnectWorkerForTest(this.workerFp, null);
  }

  throwOnSend(value: boolean): void {
    this.#throwOnSend = value;
  }

  async waitForKind(kind: string, count = 1): Promise<CapturedGlobalSearchCommand[]> {
    const matching = () => this.commands.filter((command) => command.control.kind === kind);
    if (matching().length < count) {
      const { promise, resolve, reject } = Promise.withResolvers<void>();
      let timeout: ReturnType<typeof setTimeout>;
      const check = (): void => {
        if (matching().length < count) return;
        clearTimeout(timeout);
        this.#waiters.delete(check);
        resolve();
      };
      timeout = setTimeout(() => {
        this.#waiters.delete(check);
        reject(new Error(`timed out waiting for ${kind}`));
      }, 1_000);
      this.#waiters.add(check);
      await promise;
    }
    return matching().slice(0, count);
  }

  respond(command: CapturedGlobalSearchCommand, data: unknown): void {
    if (!resolvePendingRpc(command.browserCommand.requestId, data, this.workerFp)) {
      throw new Error("global search pending RPC was not found");
    }
  }

  #capture(frame: CoordWorkerDown): number {
    if (this.#throwOnSend) throw new Error("injected global search send failure");
    if (frame.frame.case !== "browserCommand") return 1;
    const command: CapturedGlobalSearchCommand = {
      workerFp: this.workerFp,
      browserCommand: frame.frame.value,
      control: JSON.parse(frame.frame.value.frameJson) as Record<string, unknown>,
    };
    this.commands.push(command);
    for (const waiter of this.#waiters) waiter();
    return 1;
  }
}

export interface InsertGlobalSearchSessionOptions {
  id: string;
  workerFp: string;
  dashboardId?: string;
  status?: "open" | "closed";
  createdAt?: number;
}

export type GlobalSearchTestHandlers = Pick<
  ServiceImpl<typeof CoordinatorService>,
  "sessionsSearchGlobal" | "sessionsCancelGlobalSearch"
>;

export interface GlobalSearchTestFixture {
  db: ConnectDeps["db"];
  deps: ConnectDeps;
  context(options?: {
    dashboardId?: string;
    deviceFingerprint?: string;
    tabId?: string;
    signal?: AbortSignal;
  }): HandlerContext;
  handlers(
    cursorOwner?: GlobalSearchCursorOwner,
    workerLanes?: GlobalSearchWorkerLaneOwner,
  ): GlobalSearchTestHandlers;
  installWorker(workerFp: string, dashboardId?: string): GlobalSearchTestWorker;
  insertSession(options: InsertGlobalSearchSessionOptions): Promise<void>;
  reset(): Promise<void>;
  cleanup(): Promise<void>;
}

export async function startGlobalSearchTestFixture(): Promise<GlobalSearchTestFixture> {
  const workdir = mkdtempSync(join(tmpdir(), "roost-global-search-"));
  const opened = openDb(join(workdir, "coord.db"));
  await runMigrations(opened.sqlite);
  const organizationA = "global-search-organization-a";
  const organizationB = "global-search-organization-b";
  const now = Date.now();
  await opened.db.insertInto("organizations").values([
    {
      id: organizationA,
      slug: "global-a",
      name: "Global A",
      status: "active",
      created_at_ms: now,
    },
    {
      id: organizationB,
      slug: "global-b",
      name: "Global B",
      status: "active",
      created_at_ms: now,
    },
  ]).execute();
  await opened.db.insertInto("dashboards").values([
    {
      id: GLOBAL_TEST_DASHBOARD_A,
      organization_id: organizationA,
      slug: "global-a",
      name: "Global A",
      status: "active",
      created_at_ms: now,
    },
    {
      id: GLOBAL_TEST_DASHBOARD_B,
      organization_id: organizationB,
      slug: "global-b",
      name: "Global B",
      status: "active",
      created_at_ms: now,
    },
  ]).execute();
  await opened.db.insertInto("workers").values([
    {
      fp: GLOBAL_TEST_WORKER_A1,
      dashboard_id: GLOBAL_TEST_DASHBOARD_A,
      label: "worker-a1",
      os: "linux",
      git_sha: null,
      host_metrics_json: null,
      registered_at_ms: now,
      last_seen_ms: now,
      reachable_addr: null,
    },
    {
      fp: GLOBAL_TEST_WORKER_A2,
      dashboard_id: GLOBAL_TEST_DASHBOARD_A,
      label: "worker-a2",
      os: "linux",
      git_sha: null,
      host_metrics_json: null,
      registered_at_ms: now,
      last_seen_ms: now,
      reachable_addr: null,
    },
    {
      fp: GLOBAL_TEST_WORKER_B,
      dashboard_id: GLOBAL_TEST_DASHBOARD_B,
      label: "worker-b",
      os: "linux",
      git_sha: null,
      host_metrics_json: null,
      registered_at_ms: now,
      last_seen_ms: now,
      reachable_addr: null,
    },
  ]).execute();
  const deps = { db: opened.db } as unknown as ConnectDeps;
  const installedWorkers = new Set<GlobalSearchTestWorker>();

  return {
    db: opened.db,
    deps,
    context(options = {}): HandlerContext {
      const dashboardId = options.dashboardId ?? GLOBAL_TEST_DASHBOARD_A;
      const deviceFingerprint = options.deviceFingerprint ?? "global-browser";
      const values = createContextValues();
      values.set(callerKey, {
        kind: "account-device",
        fingerprint: deviceFingerprint,
        label: "Global browser",
        accountId: "global-account",
      });
      values.set(dashboardActorKey, {
        accountId: "global-account",
        organizationId: dashboardId === GLOBAL_TEST_DASHBOARD_A
          ? organizationA
          : organizationB,
        dashboardId,
        organizationRole: "owner",
        dashboardRole: "admin",
        deviceFingerprint,
      });
      values.set(tabIdKey, options.tabId ?? "global-tab");
      return {
        signal: options.signal ?? new AbortController().signal,
        values,
      } as unknown as HandlerContext;
    },
    handlers(
      cursorOwner = new GlobalSearchCursorOwner(),
      workerLanes = new GlobalSearchWorkerLaneOwner(),
    ) {
      return makeSessionGlobalSearchHandlers(deps, cursorOwner, workerLanes);
    },
    installWorker(workerFp, dashboardId = GLOBAL_TEST_DASHBOARD_A) {
      const worker = new GlobalSearchTestWorker(workerFp, dashboardId);
      worker.install();
      installedWorkers.add(worker);
      return worker;
    },
    async insertSession(options): Promise<void> {
      await opened.db.insertInto("sessions").values({
        id: options.id,
        dashboard_id: options.dashboardId ?? GLOBAL_TEST_DASHBOARD_A,
        worker_fp: options.workerFp,
        channel: Number.parseInt(options.id.slice(-6), 16),
        kind: "shell",
        cwd: "/tmp",
        status: options.status ?? "open",
        created_at: options.createdAt ?? Date.now(),
      }).execute();
    },
    async reset(): Promise<void> {
      for (const worker of installedWorkers) worker.remove();
      installedWorkers.clear();
      await opened.db.deleteFrom("sessions").execute();
      await opened.db.updateTable("workers").set({ deleted_at_ms: null }).execute();
    },
    async cleanup(): Promise<void> {
      for (const worker of installedWorkers) worker.remove();
      await opened.close();
      rmSync(workdir, { recursive: true, force: true });
    },
  };
}
