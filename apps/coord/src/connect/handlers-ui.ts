// Browser UI control handlers retain typed tab reports, relay the eight legacy
// fire-and-forget commands, and publish one socket-fenced acknowledged layout apply.
// Portable layout validation stays in the shared parser; persisted dashboard
// session ownership is checked before any report or command enters the live bus.

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  CoordinatorService,
  UiReportStateResponseSchema, UiListStatesResponseSchema,
  UiTabStateSchema, UiDispatchResponseSchema, UiApplyLayoutResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
  UiApplyLayoutSchema,
  UiCommandSchema,
  UiReportStateRequestSchema,
  type LayoutDocumentV1,
} from "@roost/shared/proto/sync_pb";
import {
  UI_ACTIVE_PATH_MAX_UTF8_BYTES,
  UI_FOLDER_KEY_MAX_UTF8_BYTES,
  UI_TAB_ID_MAX_UTF8_BYTES,
  hasAtMostUtf8Bytes,
} from "@roost/shared/ui-state";
import {
  layoutDocumentFromProto,
  layoutDocumentToProto,
} from "@roost/shared/layout-document-proto";
import { uiBus } from "../buses.ts";
import { requireDashboardActor, requireDashboardAdmin } from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";
import {
  UiLayoutApplyCanceledError,
  UiLayoutApplyCapacityError,
} from "./ui-layout-apply-owner.ts";
import {
  canonicalLegacyUiCommand,
  legacyUiCommandSessionIds,
} from "./ui-legacy-command.ts";
import {
  UiStateCapacityError,
  UiStateIdentityRateError,
} from "./ui-state-owner.ts";

async function requireDashboardSessionBindings(
  deps: ConnectDeps,
  dashboardId: string,
  sessionIdsInput: Iterable<string>,
): Promise<void> {
  const sessionIds = [...new Set(sessionIdsInput)];
  if (sessionIds.length === 0) return;
  const rows = await deps.db.selectFrom("sessions").select("id")
    .where("dashboard_id", "=", dashboardId)
    .where("id", "in", sessionIds)
    .execute();
  const found = new Set(rows.map((row) => row.id));
  if (sessionIds.some((sessionId) => !found.has(sessionId))) {
    throw new ConnectError("session not found", Code.NotFound);
  }
}

function validateLayoutDocument(document: LayoutDocumentV1 | undefined) {
  if (!document) {
    throw new ConnectError("layout document is required", Code.InvalidArgument);
  }
  try {
    return layoutDocumentFromProto(document);
  } catch {
    throw new ConnectError("invalid layout document", Code.InvalidArgument);
  }
}

function requireBoundedUiText(
  value: string,
  maxBytes: number,
  field: string,
  required: boolean,
): void {
  if (
    (required && value.trim().length === 0)
    || !hasAtMostUtf8Bytes(value, maxBytes)
  ) {
    throw new ConnectError(`invalid ${field}`, Code.InvalidArgument);
  }
}

type UiMethods = "uiReportState" | "uiListStates" | "uiDispatch" | "uiApplyLayout";
export type UiHandlers = Pick<ServiceImpl<typeof CoordinatorService>, UiMethods>;

export function makeUiHandlers(deps: ConnectDeps): UiHandlers {
  return {
    async uiReportState(req, ctx) {
      // Device fingerprint and dashboard scope both come from the verified
      // selected-dashboard actor; a tab cannot impersonate another browser or
      // overwrite the same browser-local tab in another dashboard.
      const actor = requireDashboardActor(ctx.values);
      requireBoundedUiText(
        req.tabId,
        UI_TAB_ID_MAX_UTF8_BYTES,
        "UI report tab id",
        true,
      );
      requireBoundedUiText(
        req.activePath,
        UI_ACTIVE_PATH_MAX_UTF8_BYTES,
        "UI report active path",
        false,
      );
      requireBoundedUiText(
        req.folderKey,
        UI_FOLDER_KEY_MAX_UTF8_BYTES,
        "UI report folder key",
        false,
      );
      let canonicalDocument: LayoutDocumentV1 | undefined;
      if (req.layoutDocument) {
        const checked = validateLayoutDocument(req.layoutDocument);
        await requireDashboardSessionBindings(
          deps,
          actor.dashboardId,
          checked.bindings.map((binding) => binding.session_id),
        );
        canonicalDocument = layoutDocumentToProto(checked);
      }
      const fp = actor.deviceFingerprint;
      const state = create(UiReportStateRequestSchema, {
        tabId: req.tabId,
        activePath: req.activePath,
        folderKey: req.folderKey,
        layoutDocument: canonicalDocument,
      });
      try {
        deps.uiStates.report({
          dashboardId: actor.dashboardId,
          fingerprint: fp,
          tabId: req.tabId,
          state,
        });
      } catch (error) {
        if (
          error instanceof UiStateCapacityError
          || error instanceof UiStateIdentityRateError
        ) {
          throw new ConnectError(error.message, Code.ResourceExhausted);
        }
        throw error;
      }
      uiBus.publish({
        kind: "state", fp, tabId: req.tabId, state,
        _dashboard_id: actor.dashboardId,
      });
      return create(UiReportStateResponseSchema, {});
    },

    async uiListStates(_req, ctx) {
      const actor = requireDashboardActor(ctx.values);
      const entries = deps.uiStates.list(actor.dashboardId);
      // Batch label lookup — one query for all distinct fps (small N: one
      // entry per open browser tab). "" when the fp has no authorized_keys
      // row (e.g. key revoked while the tab was still reporting).
      const fps = [...new Set(entries.map((e) => e.fp))];
      const labelByFp = new Map<string, string>();
      if (fps.length > 0) {
        const rows = await deps.db.selectFrom("authorized_keys")
          .select(["fingerprint", "label"])
          .where("fingerprint", "in", fps).execute();
        for (const r of rows) labelByFp.set(r.fingerprint, r.label);
      }
      return create(UiListStatesResponseSchema, {
        tabs: entries.map((e) => create(UiTabStateSchema, {
          fp: e.fp,
          tabId: e.tabId,
          label: labelByFp.get(e.fp) ?? "",
          lastMs: BigInt(e.lastMs),
          state: e.state,
        })),
      });
    },

    async uiDispatch(req, ctx) {
      const actor = requireDashboardAdmin(ctx.values);
      requireBoundedUiText(
        req.targetTabId,
        UI_TAB_ID_MAX_UTF8_BYTES,
        "UI dispatch target tab id",
        false,
      );
      // A UiCommand with no case set would relay as a no-op every tab
      // silently drops — reject at the wire boundary instead (symmetric to
      // tasksEnqueue's payload validation).
      const command = req.command;
      if (!command?.command.case) {
        throw new ConnectError("uiDispatch requires a command", Code.InvalidArgument);
      }
      if (command.command.case === "applyLayout") {
        throw new ConnectError(
          "uiDispatch does not accept applyLayout",
          Code.InvalidArgument,
        );
      }
      if (command.command.case === "navigate") {
        requireBoundedUiText(
          command.command.value.path,
          UI_ACTIVE_PATH_MAX_UTF8_BYTES,
          "UI navigation path",
          false,
        );
      }
      const commandSessionIds = legacyUiCommandSessionIds(command);
      await requireDashboardSessionBindings(deps, actor.dashboardId, commandSessionIds);
      const canonicalCommand = canonicalLegacyUiCommand(command);
      // Subscriber count AT publish is restricted to the selected dashboard's
      // live Sync streams. 0 tells a headless caller no one can execute this.
      const delivered = uiBus.subscriberCountFor(actor.dashboardId);
      uiBus.publish({
        kind: "command", targetTabId: req.targetTabId, command: canonicalCommand,
        _dashboard_id: actor.dashboardId,
      });
      return create(UiDispatchResponseSchema, { delivered });
    },

    async uiApplyLayout(req, ctx) {
      const actor = requireDashboardAdmin(ctx.values);
      requireBoundedUiText(
        req.targetTabId,
        UI_TAB_ID_MAX_UTF8_BYTES,
        "UI apply target tab id",
        true,
      );
      requireBoundedUiText(
        req.targetFingerprint,
        UI_TAB_ID_MAX_UTF8_BYTES,
        "UI apply target fingerprint",
        true,
      );
      const checked = validateLayoutDocument(req.document);
      await requireDashboardSessionBindings(
        deps,
        actor.dashboardId,
        checked.bindings.map((binding) => binding.session_id),
      );
      const document = layoutDocumentToProto(checked);
      const command = create(UiCommandSchema, {
        command: {
          case: "applyLayout",
          value: create(UiApplyLayoutSchema, { document }),
        },
      });
      try {
        // The admin-selected reporting fingerprint is pinned into the exact
        // live socket reservation; another device claiming the same tab id
        // cannot receive or acknowledge this apply.
        const pendingResult = deps.uiLayoutApplies.requestApply(
          actor.dashboardId,
          req.targetFingerprint,
          req.targetTabId,
          ctx.signal,
          ({ correlationId, socketId }) => {
            uiBus.publish({
              kind: "apply",
              targetTabId: req.targetTabId,
              targetSocketId: socketId,
              correlationId,
              command,
              _dashboard_id: actor.dashboardId,
            });
          },
        );
        const result = await pendingResult;
        return create(UiApplyLayoutResponseSchema, result);
      } catch (error) {
        if (error instanceof UiLayoutApplyCanceledError) {
          throw new ConnectError(error.message, Code.Canceled);
        }
        if (error instanceof UiLayoutApplyCapacityError) {
          throw new ConnectError(error.message, Code.ResourceExhausted);
        }
        throw error;
      }
    },
  };
}
