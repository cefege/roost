// MCP relay CRUD handlers: persistence plus the delta a worker subscriber sees.
// Proves create/list/publish/delete stay consistent with the mcp bus fan-out,
// that a worker JWT is refused, and that the retained dashboard column is written.

import { afterAll, beforeAll, beforeEach, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code, createContextValues, type HandlerContext } from "@connectrpc/connect";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  McpCreateRequestSchema,
  McpDeleteRequestSchema,
  McpListRequestSchema,
  McpPublishRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import type { McpStreamMessage } from "@roost/shared/wire";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { ensureSelfHostedTenant, type SelfHostedTenant } from "../src/self-hosted-tenant.ts";
import { callerKey } from "../src/connect/auth-interceptor.ts";
import { makeMcpHandlers } from "../src/connect/handlers-mcp.ts";
import { mcpBus } from "../src/buses.ts";
import type { ConnectDeps } from "../src/connect/router.ts";

const deviceFingerprint = "fp-mcp-device";
const workerFingerprint = "fp-mcp-worker";

let workdir: string;
let closeDb: () => Promise<void>;
let db: KyselyDB;
let tenant: SelfHostedTenant;

function mcpHandlers() {
  return makeMcpHandlers({ db, selfHostedTenant: tenant } as unknown as ConnectDeps);
}

function deviceContext(): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device",
    fingerprint: deviceFingerprint,
    label: "test device",
    accountId: tenant.accountId,
  });
  return { values } as unknown as HandlerContext;
}

function workerContext(): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "worker",
    fingerprint: workerFingerprint,
    label: "test worker",
  });
  return { values } as unknown as HandlerContext;
}

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-mcp-handlers-"));
  const opened = openDb(join(workdir, "test.db"));
  db = opened.db;
  closeDb = opened.close;
  await runMigrations(opened.sqlite);
  tenant = ensureSelfHostedTenant(opened.sqlite, { backfillLegacyScopes: false });
});

afterAll(async () => {
  await closeDb?.();
  rmSync(workdir, { recursive: true, force: true });
});

beforeEach(async () => {
  await db.deleteFrom("mcp_relays").execute();
});

test("create, list, publish, and delete stay consistent with the relay stream", async () => {
  const handlers = mcpHandlers();
  const messages: McpStreamMessage[] = [];
  const unsubscribe = mcpBus.subscribe((message) => messages.push(message));
  try {
    const created = await handlers.mcpCreate(create(McpCreateRequestSchema, {
      label: "Local tools",
      kind: "stdio",
      configJson: JSON.stringify({ command: "bun", args: ["run", "mcp"] }),
    }), deviceContext());
    expect(created.relay).toMatchObject({ label: "Local tools", kind: "stdio" });
    const id = created.relay!.id;

    const listed = await handlers.mcpList(create(McpListRequestSchema), deviceContext());
    expect(listed.relays?.map((relay) => relay.id)).toEqual([id]);

    await handlers.mcpPublish(create(McpPublishRequestSchema, {
      id,
      payloadJson: JSON.stringify({ method: "tools/list" }),
    }), deviceContext());

    const deleted = await handlers.mcpDelete(create(McpDeleteRequestSchema, { id }), deviceContext());
    expect(deleted.ok).toBe(true);
    expect(
      (await handlers.mcpList(create(McpListRequestSchema), deviceContext())).relays,
    ).toEqual([]);

    expect(messages).toHaveLength(3);
    expect(messages[0]).toMatchObject({
      kind: "created",
      relay: { id, label: "Local tools", kind: "stdio", config: { command: "bun" } },
    });
    expect(messages[1]).toMatchObject({ relay_id: id, payload: { method: "tools/list" } });
    expect(messages[2]).toMatchObject({ kind: "deleted", id });
  } finally {
    unsubscribe();
  }
});

test("a created relay persists the resolved singleton dashboard id", async () => {
  const handlers = mcpHandlers();
  const created = await handlers.mcpCreate(create(McpCreateRequestSchema, {
    label: "Scoped tools",
    kind: "sse",
    configJson: JSON.stringify({ url: "http://127.0.0.1:9999/sse" }),
  }), deviceContext());
  const id = created.relay?.id;
  if (id === undefined) throw new Error("create response omitted the relay");
  const row = await db.selectFrom("mcp_relays")
    .select(["id", "dashboard_id"])
    .where("id", "=", id)
    .executeTakeFirst();
  expect(row).toEqual({ id, dashboard_id: tenant.dashboardId });
});

test("an unparseable config is rejected before any row persists", async () => {
  const handlers = mcpHandlers();
  await expect(handlers.mcpCreate(create(McpCreateRequestSchema, {
    label: "Broken",
    kind: "stdio",
    configJson: "[1,2,3]",
  }), deviceContext())).rejects.toMatchObject({ code: Code.InvalidArgument });
  expect(await db.selectFrom("mcp_relays").selectAll().execute()).toEqual([]);
});

test("publish and delete of an unknown relay are not found", async () => {
  const handlers = mcpHandlers();
  await expect(handlers.mcpPublish(create(McpPublishRequestSchema, {
    id: "00000000-0000-4000-8000-00000000dead",
    payloadJson: "{}",
  }), deviceContext())).rejects.toMatchObject({ code: Code.NotFound });
  await expect(handlers.mcpDelete(create(McpDeleteRequestSchema, {
    id: "00000000-0000-4000-8000-00000000dead",
  }), deviceContext())).rejects.toMatchObject({ code: Code.NotFound });
});

test("a worker principal has no authority over the relay registry", async () => {
  const handlers = mcpHandlers();
  for (const call of [
    () => handlers.mcpList(create(McpListRequestSchema), workerContext()),
    () => handlers.mcpCreate(create(McpCreateRequestSchema, {
      label: "Worker tools",
      kind: "stdio",
      configJson: "{}",
    }), workerContext()),
    () => handlers.mcpPublish(create(McpPublishRequestSchema, {
      id: "00000000-0000-4000-8000-00000000dead",
      payloadJson: "{}",
    }), workerContext()),
    () => handlers.mcpDelete(create(McpDeleteRequestSchema, {
      id: "00000000-0000-4000-8000-00000000dead",
    }), workerContext()),
  ]) {
    await expect(call()).rejects.toMatchObject({ code: Code.Unauthenticated });
  }
  expect(await db.selectFrom("mcp_relays").selectAll().execute()).toEqual([]);
});
