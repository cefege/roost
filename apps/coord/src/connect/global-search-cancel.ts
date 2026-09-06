// Owns explicit coordinator cancellation for dashboard-wide terminal search.
// The session handler composes this function into its single handler domain;
// cursor tombstones are installed before any asynchronous worker discovery.

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  CoordinatorService,
  SessionsCancelGlobalSearchResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
  GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
  TerminalSearchIdSchema,
} from "@roost/shared/terminal-search";
import {
  requireAccountDevice,
  requireDashboardActor,
  tabIdKey,
} from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";
import {
  listAuthorizedGlobalSearchSessions,
  sendGlobalSearchCancellationBatches,
} from "./global-search-fanout.ts";
import {
  GlobalSearchCursorOwner,
  type GlobalSearchIdentity,
  type GlobalSearchSessionPosition,
} from "./global-search-cursors.ts";

type CancelGlobalSearchHandler = ServiceImpl<
  typeof CoordinatorService
>["sessionsCancelGlobalSearch"];

export function makeGlobalSearchCancelHandler(
  deps: ConnectDeps,
  cursorOwner: GlobalSearchCursorOwner,
): CancelGlobalSearchHandler {
  return async (req, ctx) => {
    const actor = requireDashboardActor(ctx.values);
    const caller = requireAccountDevice(ctx.values);
    if (!TerminalSearchIdSchema.safeParse(req.searchId).success) {
      throw new ConnectError(
        "global search search_id must contain 1 to 64 characters",
        Code.InvalidArgument,
      );
    }
    const tabId = ctx.values.get(tabIdKey) ?? "";
    const identity: GlobalSearchIdentity = {
      dashboardId: actor.dashboardId,
      deviceFingerprint: caller.fingerprint,
      tabId,
      searchId: req.searchId,
    };
    const cancellation = cursorOwner.prepareCancellation(identity);
    if (cancellation.shouldDispatch) {
      const viewerId = tabId ? `${caller.fingerprint}:${tabId}` : caller.fingerprint;
      let cancellationQueued = false;
      try {
        const current = await listAuthorizedGlobalSearchSessions(
          deps.db,
          actor.dashboardId,
          GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
        );
        const merged = new Map<string, GlobalSearchSessionPosition>();
        for (const session of [...cancellation.selectedSessions, ...current]) {
          if (merged.size >= GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS) break;
          if (!merged.has(session.sessionId)) merged.set(session.sessionId, session);
        }
        sendGlobalSearchCancellationBatches(
          caller,
          viewerId,
          req.searchId,
          [...merged.values()],
        );
        cancellationQueued = true;
      } finally {
        if (!cancellationQueued) {
          sendGlobalSearchCancellationBatches(
            caller,
            viewerId,
            req.searchId,
            cancellation.selectedSessions,
          );
        }
        cursorOwner.completeCancellation(identity);
      }
    }
    return create(SessionsCancelGlobalSearchResponseSchema, {});
  };
}
