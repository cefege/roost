// Audit persistence batching and terminal-input completion guarantees.
// These tests hold transactions open at the commit boundary so no result or
// audit event can pass durable storage, and exercise capacity after rollback.

import { afterAll, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setSignalSink } from "@roost/shared/diag";
import { auditBus } from "../src/buses.ts";
import { processInputControl, type InputControlResult } from "../src/connect/input-control.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { openDb, type DbHandle, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import {
  ensureSelfHostedTenant,
  type SelfHostedTenant,
} from "../src/self-hosted-tenant.ts";
import {
  writeAuditLog,
  writeAuditLogs,
  type AuditLogOptions,
} from "../src/middleware/security.ts";

interface TestGate {
  promise: Promise<void>;
  resolve: () => void;
}

interface AuditTransactionGate {
  db: KyselyDB;
  entered: TestGate[];
  releases: TestGate[];
  state: { transactions: number };
}

let workdir = "";
let primary: DbHandle;
let secondary: DbHandle;
let reader: DbHandle;
let primaryTenant: SelfHostedTenant;
let secondaryTenant: SelfHostedTenant;

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-input-audit-batching-"));
  const primaryPath = join(workdir, "primary.db");
  primary = openDb(primaryPath);
  await runMigrations(primary.sqlite);
  primaryTenant = ensureSelfHostedTenant(primary.sqlite, { backfillLegacyScopes: false });
  reader = openDb(primaryPath);

  secondary = openDb(join(workdir, "secondary.db"));
  await runMigrations(secondary.sqlite);
  secondaryTenant = ensureSelfHostedTenant(secondary.sqlite, { backfillLegacyScopes: false });
});

beforeEach(async () => {
  await primary.db.deleteFrom("audit_log").execute();
  await secondary.db.deleteFrom("audit_log").execute();
});

afterAll(async () => {
  try {
    await reader.close();
    await secondary.close();
    await primary.close();
  } finally {
    if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true });
  }
});

function gateAuditTransactionsBeforeCommit(
  db: KyselyDB,
  gateCount: number,
  failFirst: boolean,
): AuditTransactionGate {
  const entered: TestGate[] = [];
  const releases: TestGate[] = [];
  for (let index = 0; index < gateCount; index += 1) {
    const entry = Promise.withResolvers<void>();
    const release = Promise.withResolvers<void>();
    entered.push({ promise: entry.promise, resolve: entry.resolve });
    releases.push({ promise: release.promise, resolve: release.resolve });
  }
  const state = { transactions: 0 };
  const gatedDb = {
    transaction() {
      return {
        async execute(callback: (transaction: KyselyDB) => Promise<unknown>): Promise<unknown> {
          const transactionIndex = state.transactions;
          state.transactions += 1;
          return db.transaction().execute(async (transaction) => {
            const result = await callback(transaction as unknown as KyselyDB);
            if (transactionIndex < gateCount) {
              entered[transactionIndex]!.resolve();
              await releases[transactionIndex]!.promise;
            }
            if (failFirst && transactionIndex === 0) {
              throw new Error("injected audit transaction failure");
            }
            return result;
          });
        },
      };
    },
  } as unknown as KyselyDB;
  return { db: gatedDb, entered, releases, state };
}

function startAuditedInputs(
  deps: ConnectDeps,
  count: number,
  label: string,
  firstIndex: number,
  settled: boolean[],
  rejectedIndex?: number,
): Promise<InputControlResult>[] {
  const requests: Promise<InputControlResult>[] = [];
  for (let index = 0; index < count; index += 1) {
    const resultIndex = firstIndex + index;
    const data = resultIndex === rejectedIndex
      ? new Uint8Array(64 * 1024 + 1)
      : new Uint8Array();
    const request = processInputControl(deps, {
      identity: {
        viewerKey: `${label}-viewer-${resultIndex}`,
        callerFingerprint: `${label}-caller-${resultIndex}`,
      },
      sessionId: `${label}-session-${resultIndex}`,
      inputSeq: BigInt(resultIndex + 1),
      data,
      audit: { traceId: `${label}-trace-${resultIndex}` },
    }).then((result) => {
      settled[resultIndex] = true;
      return result;
    });
    requests.push(request);
  }
  return requests;
}

function releaseTransactionGates(gate: AuditTransactionGate): void {
  for (const release of gate.releases) release.resolve();
}

describe("audit log batches", () => {
  test("publishes returned rows in committed ID order and retains the single-row API", async () => {
    const prefix = "/audit-batch/committed";
    const paths = [`${prefix}/three`, `${prefix}/one`, `${prefix}/two`];
    const options: AuditLogOptions[] = [];
    for (let index = 0; index < paths.length; index += 1) {
      options.push({
        db: primary.db,
        status: 200 + index,
        method: "SYNC",
        path: paths[index]!,
        traceId: `trace-${index}`,
        callerFp: `caller-${index}`,
        dashboardId: primaryTenant.dashboardId,
        recordTelemetry: false,
      });
    }
    const publishedIds: number[] = [];
    const publishedPaths: string[] = [];
    const visibleAtPublication: number[] = [];
    const unsubscribe = auditBus.subscribe((row) => {
      if (!row.path.startsWith(prefix)) return;
      publishedIds.push(row.id);
      publishedPaths.push(row.path);
      const visible = reader.sqlite.query("SELECT id FROM audit_log WHERE id = ?").get(row.id);
      if (visible && typeof visible === "object" && "id" in visible) {
        visibleAtPublication.push(Number(visible.id));
      }
    });
    try {
      await writeAuditLogs(options);
      const singlePath = `${prefix}/single`;
      await writeAuditLog({
        db: primary.db,
        status: 204,
        method: "SYNC",
        path: singlePath,
        traceId: "trace-single",
        callerFp: "caller-single",
        dashboardId: primaryTenant.dashboardId,
        recordTelemetry: false,
      });
      paths.push(singlePath);

      const rows = await primary.db.selectFrom("audit_log")
        .select(["id", "path"])
        .where("path", "in", paths)
        .orderBy("id")
        .execute();
      const rowIds: number[] = [];
      const rowPaths: string[] = [];
      for (const row of rows) {
        rowIds.push(row.id!);
        rowPaths.push(row.path);
      }
      expect(rowPaths).toEqual(paths);
      expect(publishedPaths).toEqual(paths);
      expect(publishedIds).toEqual(rowIds);
      expect(visibleAtPublication).toEqual(rowIds);
    } finally {
      unsubscribe();
    }
  });

  test("rejects a mixed-database audit batch before writing either database", async () => {
    const primaryPath = "/audit-batch/mixed-primary";
    const secondaryPath = "/audit-batch/mixed-secondary";
    await expect(writeAuditLogs([
      {
        db: primary.db,
        status: 200,
        method: "SYNC",
        path: primaryPath,
        traceId: undefined,
        callerFp: "primary-caller",
        dashboardId: primaryTenant.dashboardId,
      },
      {
        db: secondary.db,
        status: 200,
        method: "SYNC",
        path: secondaryPath,
        traceId: undefined,
        callerFp: "secondary-caller",
        dashboardId: secondaryTenant.dashboardId,
      },
    ])).rejects.toThrow("audit batch must use one database");

    const primaryRows = await primary.db.selectFrom("audit_log")
      .select("id")
      .where("path", "=", primaryPath)
      .execute();
    const secondaryRows = await secondary.db.selectFrom("audit_log")
      .select("id")
      .where("path", "=", secondaryPath)
      .execute();
    expect(primaryRows).toHaveLength(0);
    expect(secondaryRows).toHaveLength(0);
  });
});

describe("input audit pump", () => {
  test("commits same-database FIFO prefixes of 64 before moving to another database", async () => {
    const primaryGate = gateAuditTransactionsBeforeCommit(primary.db, 2, false);
    const secondaryGate = gateAuditTransactionsBeforeCommit(secondary.db, 1, false);
    const primaryDeps = {
      db: primaryGate.db,
      selfHostedTenant: primaryTenant,
    } as ConnectDeps;
    const secondaryDeps = {
      db: secondaryGate.db,
      selfHostedTenant: secondaryTenant,
    } as ConnectDeps;
    const primaryPublicationIds: number[] = [];
    const primaryPublicationCallers: string[] = [];
    const primaryPublicationPaths: string[] = [];
    const unsubscribe = auditBus.subscribe((row) => {
      if (!row.caller_fp?.startsWith("primary-prefix-caller-")) return;
      primaryPublicationIds.push(row.id);
      primaryPublicationCallers.push(row.caller_fp);
      primaryPublicationPaths.push(row.path);
    });
    const settled = new Array<boolean>(66).fill(false);
    const requests = startAuditedInputs(primaryDeps, 64, "primary-prefix", 0, settled);
    requests.push(...startAuditedInputs(secondaryDeps, 1, "secondary-middle", 64, settled));
    requests.push(...startAuditedInputs(primaryDeps, 1, "primary-tail", 65, settled));
    try {
      await primaryGate.entered[0]!.promise;
      expect(settled).toEqual(new Array<boolean>(66).fill(false));
      const uncommittedRows = reader.sqlite.query("SELECT id FROM audit_log").all();
      expect(uncommittedRows).toHaveLength(0);
      expect(primaryPublicationIds).toEqual([]);

      primaryGate.releases[0]!.resolve();
      await secondaryGate.entered[0]!.promise;
      await Promise.resolve();
      const firstPrefixRows = reader.sqlite.query(
        "SELECT id, caller_fp FROM audit_log ORDER BY id",
      ).all() as Array<{ id: number; caller_fp: string }>;
      const expectedFirstPrefix: string[] = [];
      const firstPrefixIds: number[] = [];
      const firstPrefixCallers: string[] = [];
      for (let index = 0; index < 64; index += 1) {
        expectedFirstPrefix.push(`primary-prefix-caller-${index}`);
      }
      for (const row of firstPrefixRows) {
        firstPrefixIds.push(row.id);
        firstPrefixCallers.push(row.caller_fp);
      }
      expect(firstPrefixRows).toHaveLength(64);
      expect(firstPrefixCallers).toEqual(expectedFirstPrefix);
      expect(primaryPublicationIds).toEqual(firstPrefixIds);
      expect(primaryPublicationCallers).toEqual(expectedFirstPrefix);
      expect(primaryPublicationPaths).toEqual(
        new Array<string>(64).fill("/ws/coord-sync/input/accepted/0/SessionsInput"),
      );
      expect(settled[0]).toBe(true);
      expect(settled[64]).toBe(false);

      secondaryGate.releases[0]!.resolve();
      await primaryGate.entered[1]!.promise;
      const secondaryRows = await secondary.db.selectFrom("audit_log")
        .select("caller_fp")
        .orderBy("id")
        .execute();
      expect(secondaryRows).toEqual([{ caller_fp: "secondary-middle-caller-64" }]);
      const beforeTailRows = reader.sqlite.query("SELECT id FROM audit_log").all();
      expect(beforeTailRows).toHaveLength(64);

      primaryGate.releases[1]!.resolve();
      const outcomes = await Promise.all(requests);
      const statuses: string[] = [];
      for (const outcome of outcomes) statuses.push(outcome.status);
      expect(statuses).toEqual(new Array<string>(66).fill("accepted"));
      expect(settled).toEqual(new Array<boolean>(66).fill(true));
      expect(primaryGate.state.transactions).toBe(2);
      expect(secondaryGate.state.transactions).toBe(1);
    } finally {
      releaseTransactionGates(primaryGate);
      releaseTransactionGates(secondaryGate);
      await Promise.allSettled(requests);
      unsubscribe();
    }
  });

  test("holds all 1,024 audit slots through rollback and preserves rejected and ambiguous outcomes", async () => {
    const gate = gateAuditTransactionsBeforeCommit(primary.db, 1, true);
    const deps = {
      db: gate.db,
      selfHostedTenant: primaryTenant,
    } as ConnectDeps;
    const settled = new Array<boolean>(1_025).fill(false);
    const capturedSignals: Array<Record<string, unknown>> = [];
    function captureSignal(record: Record<string, unknown>): void {
      capturedSignals.push(record);
    }
    setSignalSink(captureSignal);
    const requests = startAuditedInputs(deps, 1_024, "capacity", 0, settled, 0);
    try {
      await gate.entered[0]!.promise;
      requests.push(...startAuditedInputs(deps, 1, "capacity", 1_024, settled));
      await Promise.resolve();
      const backpressureSignals: Array<Record<string, unknown>> = [];
      for (const record of capturedSignals) {
        if (record.evt === "audit.input_queue_backpressure") backpressureSignals.push(record);
      }
      expect(backpressureSignals).toHaveLength(1);
      expect(backpressureSignals[0]!.caller_fp).toBe("capacity-caller-1024");
      expect(settled).toEqual(new Array<boolean>(1_025).fill(false));
      const uncommittedRows = reader.sqlite.query("SELECT id FROM audit_log").all();
      expect(uncommittedRows).toHaveLength(0);
      gate.releases[0]!.resolve();
      const outcomes = await Promise.all(requests);

      const failedStatuses: string[] = [];
      for (let index = 0; index < 64; index += 1) {
        failedStatuses.push(outcomes[index]!.status);
      }
      const expectedFailedStatuses = new Array<string>(63).fill("ambiguous");
      expectedFailedStatuses.unshift("rejected");
      expect(failedStatuses).toEqual(expectedFailedStatuses);
      const rejectedFailure = outcomes[0]!;
      if (rejectedFailure.status !== "rejected") throw new Error("expected a rejected input result");
      expect(rejectedFailure.reason).toContain("input audit persistence failed");
      const ambiguousFailure = outcomes[1]!;
      if (ambiguousFailure.status !== "ambiguous") throw new Error("expected an ambiguous input result");
      expect(ambiguousFailure.reason).toContain("input audit persistence failed");

      const recoveredStatuses: string[] = [];
      for (let index = 64; index < outcomes.length; index += 1) {
        recoveredStatuses.push(outcomes[index]!.status);
      }
      expect(recoveredStatuses).toEqual(new Array<string>(961).fill("accepted"));
      expect(settled).toEqual(new Array<boolean>(1_025).fill(true));
      const persistedRows = await primary.db.selectFrom("audit_log").select("id").execute();
      expect(persistedRows).toHaveLength(961);
    } finally {
      releaseTransactionGates(gate);
      await Promise.allSettled(requests);
      setSignalSink(null);
    }
  });
});
