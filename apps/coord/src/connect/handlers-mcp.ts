// MCP relay CRUD and publication handlers.
// Spread into router.ts's single router.service() literal.
// The retired webhook/permission settings never share this authority surface.

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import { randomUUID } from "node:crypto";
import {
  CoordinatorService,
  McpListResponseSchema, McpCreateResponseSchema,
  McpDeleteResponseSchema, McpPublishResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { McpRelaySchema } from "@roost/shared/proto/wire_pb";
import { mcpRelayRowToProto } from "@roost/shared/wire/row-proto";
import { mcpBus } from "../buses.ts";
import { requireDashboardAdmin } from "./auth-interceptor.ts";
import { requireNonEmpty } from "./router-helpers.ts";
import { McpRelayKind, McpRelayId } from "@roost/shared/wire";
import type { McpRelayKind as McpRelayKindValue } from "@roost/shared/wire";
import type { ConnectDeps } from "./router.ts";

function mcpRelayKindOf(raw: string): McpRelayKindValue {
  const parsed = McpRelayKind.safeParse(raw);
  if (!parsed.success) {
    throw new ConnectError(`invalid relay kind ${JSON.stringify(raw)}`, Code.InvalidArgument);
  }
  return parsed.data;
}

type McpMethods = "mcpList" | "mcpCreate" | "mcpDelete" | "mcpPublish";

export function makeMcpHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, McpMethods> {
  return {
    async mcpList(_req, ctx) {
      const actor = requireDashboardAdmin(ctx.values);
      const rows = await deps.db
        .selectFrom("mcp_relays")
        .selectAll()
        .where("dashboard_id", "=", actor.dashboardId)
        .execute();
      return create(McpListResponseSchema, { relays: rows.map(mcpRelayRowToProto) });
    },

    async mcpCreate(req, ctx) {
      const actor = requireDashboardAdmin(ctx.values);
      requireNonEmpty(req.label, "label");
      // Validate configJson is parseable BEFORE INSERT. Prior shape
      // ran JSON.parse only inside mcpBus.publish AFTER the INSERT
      // committed → malformed JSON left a permanent DB row + the
      // handler 500'd before publishing, so the SPA never got the
      // `created` delta. Split-brain.
      let configParsed: Record<string, unknown>;
      try {
        const v = JSON.parse(req.configJson);
        if (v === null || typeof v !== "object" || Array.isArray(v)) {
          throw new Error("configJson must be a JSON object");
        }
        configParsed = v as Record<string, unknown>;
      } catch (e) {
        throw new ConnectError(`invalid configJson: ${(e as Error).message}`, Code.InvalidArgument);
      }
      const id = randomUUID();
      const now = Date.now();
      await deps.db.insertInto("mcp_relays").values({
        dashboard_id: actor.dashboardId,
        id, label: req.label, kind: mcpRelayKindOf(req.kind),
        config_json: req.configJson, created_at_ms: now,
      }).execute();
      const relay = create(McpRelaySchema, {
        id, label: req.label, kind: req.kind, configJson: req.configJson,
        createdAtMs: BigInt(now),
      });
      mcpBus.publish({
        kind: "created",
        _dashboard_id: actor.dashboardId,
        relay: {
          id: McpRelayId.parse(id), label: req.label, kind: mcpRelayKindOf(req.kind),
          config: configParsed, created_at_ms: now,
        },
      });
      return create(McpCreateResponseSchema, { relay });
    },

    async mcpDelete(req, ctx) {
      const actor = requireDashboardAdmin(ctx.values);
      const result = await deps.db
        .deleteFrom("mcp_relays")
        .where("id", "=", req.id)
        .where("dashboard_id", "=", actor.dashboardId)
        .returningAll()
        .executeTakeFirst();
      if (!result) throw new ConnectError("not found", Code.NotFound);
      mcpBus.publish({
        kind: "deleted",
        id: McpRelayId.parse(result.id),
        _dashboard_id: actor.dashboardId,
      });
      return create(McpDeleteResponseSchema, { ok: true });
    },

    async mcpPublish(req, ctx) {
      const actor = requireDashboardAdmin(ctx.values);
      let payload: unknown;
      try { payload = JSON.parse(req.payloadJson); }
      catch (e) {
        throw new ConnectError(`invalid payloadJson: ${(e as Error).message}`, Code.InvalidArgument);
      }
      const relay = await deps.db
        .selectFrom("mcp_relays")
        .select("id")
        .where("id", "=", req.id)
        .where("dashboard_id", "=", actor.dashboardId)
        .executeTakeFirst();
      if (!relay) throw new ConnectError("not found", Code.NotFound);
      mcpBus.publish({
        relay_id: McpRelayId.parse(relay.id),
        payload,
        ts: Date.now(),
        _dashboard_id: actor.dashboardId,
      });
      return create(McpPublishResponseSchema, { ok: true });
    },
  };
}
