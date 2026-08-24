// Settings-CRUD RPC handlers: webhook tokens, permission rules, MCP relays.
// Each is straight DB CRUD over deps.db + a bus publish for the SPA delta.
// Spread into router.ts's single router.service() literal. Split out of
// router.ts (400-line cap).

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import { randomUUID } from "node:crypto";
import {
  CoordinatorService,
  WebhookTokensListResponseSchema, WebhookTokensMintResponseSchema,
  WebhookTokensDeleteResponseSchema,
  PermissionsListResponseSchema,
  PermissionsCreateResponseSchema, PermissionsUpdateResponseSchema,
  PermissionsDeleteResponseSchema,
  McpListResponseSchema, McpCreateResponseSchema,
  McpDeleteResponseSchema, McpPublishResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
  WebhookTokenMintSchema, PermissionRuleSchema, McpRelaySchema,
} from "@roost/shared/proto/wire_pb";
import {
  webhookTokenRowToProto, permissionRuleRowToProto, mcpRelayRowToProto,
} from "@roost/shared/wire/row-proto";
import { webhookBus, permissionBus, mcpBus } from "../buses.ts";
import { requireAuth } from "./auth-interceptor.ts";
import { requireNonEmpty, sha256hex } from "./router-helpers.ts";
import {
  PermissionDecision,
  McpRelayKind,
  McpRelayId,
  WebhookScope,
  asWebhookTokenId,
  asPermissionRuleId,
} from "@roost/shared/wire";
import type { PermissionDecision as PermissionDecisionValue } from "@roost/shared/wire";
import type { McpRelayKind as McpRelayKindValue } from "@roost/shared/wire";
import type { WebhookScope as WebhookScopeValue } from "@roost/shared/wire";
import type { ConnectDeps } from "./router.ts";

// Proto fields arrive as bare strings; rows and bus deltas below speak the
// shared Zod unions. Narrow ONCE at handler entry so an unknown value is an
// InvalidArgument here instead of a garbage row plus a delta every SPA must
// survive. Re-validating row reads also fences hand-edited DB values.
function permissionDecisionOf(raw: string): PermissionDecisionValue {
  const parsed = PermissionDecision.safeParse(raw);
  if (!parsed.success) {
    throw new ConnectError(`invalid decision ${JSON.stringify(raw)}`, Code.InvalidArgument);
  }
  return parsed.data;
}

function mcpRelayKindOf(raw: string): McpRelayKindValue {
  const parsed = McpRelayKind.safeParse(raw);
  if (!parsed.success) {
    throw new ConnectError(`invalid relay kind ${JSON.stringify(raw)}`, Code.InvalidArgument);
  }
  return parsed.data;
}

function webhookScopesOf(raw: string[]): WebhookScopeValue[] {
  const scopes = raw.map((scope) => WebhookScope.safeParse(scope))
    .map((parsed) => {
      if (!parsed.success) {
        throw new ConnectError(`invalid scope ${JSON.stringify(parsed.error.issues[0])}`, Code.InvalidArgument);
      }
      return parsed.data;
    });
  if (scopes.length === 0) throw new ConnectError("at least one scope is required", Code.InvalidArgument);
  return scopes;
}

type SettingsMethods =
  | "webhookTokensList" | "webhookTokensMint" | "webhookTokensDelete"
  | "permissionsList" | "permissionsCreate" | "permissionsUpdate" | "permissionsDelete"
  | "mcpList" | "mcpCreate" | "mcpDelete" | "mcpPublish";

export function makeSettingsHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, SettingsMethods> {
  return {
    // ─── webhook tokens ──────────────────────────────────────────────
    async webhookTokensList(_req, ctx) {
      requireAuth(ctx.values);
      const rows = await deps.db.selectFrom("webhook_tokens").selectAll().execute();
      return create(WebhookTokensListResponseSchema, { tokens: rows.map(webhookTokenRowToProto) });
    },

    async webhookTokensMint(req, ctx) {
      requireAuth(ctx.values);
      requireNonEmpty(req.label, "label");
      requireNonEmpty(req.scopes, "scopes");
      const id = randomUUID();
      const now = Date.now();
      const randomBytes = new Uint8Array(32);
      crypto.getRandomValues(randomBytes);
      const hex = Array.from(randomBytes).map(b => b.toString(16).padStart(2, "0")).join("");
      const plaintext = `roost_wh_${hex}`;
      const last4 = plaintext.slice(-4);
      const hash = await sha256hex(plaintext);
      await deps.db.insertInto("webhook_tokens").values({
        id, label: req.label, hash, last4,
        scopes_json: JSON.stringify(req.scopes),
        created_at_ms: now, last_used_at_ms: null,
      }).execute();
      webhookBus.publish({ kind: "created", token: {
        id: asWebhookTokenId(id), label: req.label, last4,
        scopes: webhookScopesOf(req.scopes),
        created_at_ms: now, last_used_at_ms: null,
      }});
      return create(WebhookTokensMintResponseSchema, {
        token: create(WebhookTokenMintSchema, {
          id, label: req.label, plaintext, scopes: req.scopes,
          createdAtMs: BigInt(now),
        }),
      });
    },

    async webhookTokensDelete(req, ctx) {
      requireAuth(ctx.values);
      const result = await deps.db.deleteFrom("webhook_tokens").where("id", "=", req.id).returningAll().executeTakeFirst();
      if (!result) throw new ConnectError("not found", Code.NotFound);
      webhookBus.publish({ kind: "deleted", id: asWebhookTokenId(result.id) });
      return create(WebhookTokensDeleteResponseSchema, { ok: true });
    },

    // ─── permissions ─────────────────────────────────────────────────
    async permissionsList(_req, ctx) {
      requireAuth(ctx.values);
      const rows = await deps.db.selectFrom("permission_rules").selectAll().orderBy("created_at_ms").execute();
      return create(PermissionsListResponseSchema, { rules: rows.map(permissionRuleRowToProto) });
    },

    async permissionsCreate(req, ctx) {
      requireAuth(ctx.values);
      requireNonEmpty(req.toolPattern, "toolPattern");
      requireNonEmpty(req.folderGlob, "folderGlob");
      const id = randomUUID();
      const now = Date.now();
      const enabled = req.enabled ?? true;
      await deps.db.insertInto("permission_rules").values({
        id, tool_pattern: req.toolPattern, folder_glob: req.folderGlob,
        decision: permissionDecisionOf(req.decision), enabled: enabled ? 1 : 0,
        created_at_ms: now,
      }).execute();
      const rule = create(PermissionRuleSchema, {
        id, toolPattern: req.toolPattern, folderGlob: req.folderGlob,
        decision: req.decision, enabled, createdAtMs: BigInt(now),
      });
      permissionBus.publish({ kind: "created", rule: {
        id: asPermissionRuleId(id), tool_pattern: req.toolPattern, folder_glob: req.folderGlob,
        decision: permissionDecisionOf(req.decision), enabled, created_at_ms: now,
      }});
      return create(PermissionsCreateResponseSchema, { rule });
    },

    async permissionsUpdate(req, ctx) {
      requireAuth(ctx.values);
      const result = await deps.db.updateTable("permission_rules").set({
        ...(req.toolPattern !== undefined && { tool_pattern: req.toolPattern }),
        ...(req.folderGlob !== undefined && { folder_glob: req.folderGlob }),
        ...(req.decision !== undefined && { decision: permissionDecisionOf(req.decision) }),
        ...(req.enabled !== undefined && { enabled: req.enabled ? 1 : 0 }),
      }).where("id", "=", req.id).returningAll().executeTakeFirst();
      if (!result) throw new ConnectError("not found", Code.NotFound);
      const rule = permissionRuleRowToProto(result);
      permissionBus.publish({ kind: "updated", rule: {
        id: asPermissionRuleId(result.id), tool_pattern: result.tool_pattern,
        folder_glob: result.folder_glob, decision: permissionDecisionOf(result.decision),
        enabled: result.enabled === 1, created_at_ms: result.created_at_ms,
      }});
      return create(PermissionsUpdateResponseSchema, { rule });
    },

    async permissionsDelete(req, ctx) {
      requireAuth(ctx.values);
      const result = await deps.db.deleteFrom("permission_rules").where("id", "=", req.id).returningAll().executeTakeFirst();
      if (!result) throw new ConnectError("not found", Code.NotFound);
      permissionBus.publish({ kind: "deleted", id: asPermissionRuleId(result.id) });
      return create(PermissionsDeleteResponseSchema, { ok: true });
    },

    // ─── mcp ──────────────────────────────────────────────────────────
    async mcpList(_req, ctx) {
      requireAuth(ctx.values);
      const rows = await deps.db.selectFrom("mcp_relays").selectAll().execute();
      return create(McpListResponseSchema, { relays: rows.map(mcpRelayRowToProto) });
    },

    async mcpCreate(req, ctx) {
      requireAuth(ctx.values);
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
        id, label: req.label, kind: mcpRelayKindOf(req.kind),
        config_json: req.configJson, created_at_ms: now,
      }).execute();
      const relay = create(McpRelaySchema, {
        id, label: req.label, kind: req.kind, configJson: req.configJson,
        createdAtMs: BigInt(now),
      });
      mcpBus.publish({ kind: "created", relay: {
        id: McpRelayId.parse(id), label: req.label, kind: mcpRelayKindOf(req.kind),
        config: configParsed, created_at_ms: now,
      }});
      return create(McpCreateResponseSchema, { relay });
    },

    async mcpDelete(req, ctx) {
      requireAuth(ctx.values);
      const result = await deps.db.deleteFrom("mcp_relays").where("id", "=", req.id).returningAll().executeTakeFirst();
      if (!result) throw new ConnectError("not found", Code.NotFound);
      mcpBus.publish({ kind: "deleted", id: McpRelayId.parse(result.id) });
      return create(McpDeleteResponseSchema, { ok: true });
    },

    async mcpPublish(req, ctx) {
      requireAuth(ctx.values);
      let payload: unknown;
      try { payload = JSON.parse(req.payloadJson); }
      catch (e) {
        throw new ConnectError(`invalid payloadJson: ${(e as Error).message}`, Code.InvalidArgument);
      }
      const relay = await deps.db.selectFrom("mcp_relays").select("id").where("id", "=", req.id).executeTakeFirst();
      if (!relay) throw new ConnectError("not found", Code.NotFound);
      mcpBus.publish({ relay_id: McpRelayId.parse(req.id), payload, ts: Date.now() });
      return create(McpPublishResponseSchema, { ok: true });
    },
  };
}
