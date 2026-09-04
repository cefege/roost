// ui-cc RPC handlers: uiReportState / uiListStates / uiDispatch. The spatial
// model (pane-layout tree, active route, focused pane) STAYS browser-local —
// coord never persists or interprets it, it only relays. Each live SPA tab
// heartbeats its state here (60s + on layout/route change); agents read the
// snapshot via uiListStates or the Sync ui_state frames, and drive the UI via
// uiDispatch → ui_command frames that the live tab executes with its existing
// pure layout ops. No browser open → commands land nowhere (delivered=0) and
// the state list is empty; that's the accepted trade of the command-channel
// design (plan G1 decision — no server-persisted layout, no cross-device merge).
//
// Module-level dashboard-scoped state + TTL reap for ephemeral presence-class
// data. This is an in-memory UI projection, not terminal view membership or a
// DB table. Its five-minute TTL is generous relative to the SPA heartbeat, so a
// closed tab ages out without expiring a live one.

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  CoordinatorService,
  UiReportStateResponseSchema, UiListStatesResponseSchema,
  UiTabStateSchema, UiDispatchResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import type { UiCommand, UiReportStateRequest } from "@roost/shared/proto/sync_pb";
import { uiBus } from "../buses.ts";
import { requireDashboardActor, requireDashboardAdmin } from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";

export const UI_STATE_TTL_MS = 5 * 60_000;
const UI_REAP_INTERVAL_MS = 60_000;

interface UiTabEntry {
  dashboardId: string;
  fp: string;
  tabId: string;
  lastMs: number;
  state: UiReportStateRequest;
}

// Keyed `${dashboardId}:${fp}:${tabId}` because the browser-local tab id is
// not globally unique, including across dashboards. Tests and diagnostics may
// inspect the underscored map; handlers own it.
export const _uiStatesByTab = new Map<string, UiTabEntry>();

// Inline reap shared by the interval, uiListStates, and the Sync-connect
// snapshot — all three must agree on what "live" means or a dead tab could
// be seeded to a fresh Sync stream yet missing from uiListStates.
function reapStaleUiStates(now: number): void {
  for (const [key, e] of _uiStatesByTab) {
    if (now - e.lastMs > UI_STATE_TTL_MS) _uiStatesByTab.delete(key);
  }
}

// Reaper cadence = the SPA heartbeat interval; with TTL at 5× heartbeat a
// live tab would need 5 consecutive missed reports to be dropped.
setInterval(() => reapStaleUiStates(Date.now()), UI_REAP_INTERVAL_MS).unref?.();

/** Current live tab states for one Sync dashboard seed (sync-feed.ts).
 *  Skips stale entries the interval reaper hasn't swept yet. */
export function getUiStateSnapshot(
  dashboardId: string,
): Array<{ fp: string; tabId: string; state: UiReportStateRequest }> {
  const now = Date.now();
  const out: Array<{ fp: string; tabId: string; state: UiReportStateRequest }> = [];
  for (const e of _uiStatesByTab.values()) {
    if (e.dashboardId !== dashboardId || now - e.lastMs > UI_STATE_TTL_MS) continue;
    out.push({ fp: e.fp, tabId: e.tabId, state: e.state });
  }
  return out;
}

function uiCommandSessionIds(command: UiCommand): string[] {
  const c = command.command;
  switch (c.case) {
    case "placeSplit":
      return [c.value.sessionId, c.value.anchorSessionId];
    case "selectTab":
    case "focusPane":
    case "closeTab":
    case "spotlight":
      return [c.value.sessionId];
    case "moveTab":
      return [c.value.sessionId, c.value.destSessionId];
    case "navigate":
    case "arrange":
    case undefined:
      return [];
  }
}

async function requireDashboardCommandSessions(
  deps: ConnectDeps,
  dashboardId: string,
  command: UiCommand,
): Promise<void> {
  const sessionIds = [...new Set(uiCommandSessionIds(command))];
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

type UiMethods = "uiReportState" | "uiListStates" | "uiDispatch";
export type UiHandlers = Pick<ServiceImpl<typeof CoordinatorService>, UiMethods>;

export function makeUiHandlers(deps: ConnectDeps): UiHandlers {
  return {
    async uiReportState(req, ctx) {
      // Device fingerprint and dashboard scope both come from the verified
      // selected-dashboard actor; a tab cannot impersonate another browser or
      // overwrite the same browser-local tab in another dashboard.
      const actor = requireDashboardActor(ctx.values);
      const fp = actor.deviceFingerprint;
      _uiStatesByTab.set(`${actor.dashboardId}:${fp}:${req.tabId}`, {
        dashboardId: actor.dashboardId,
        fp, tabId: req.tabId, lastMs: Date.now(), state: req,
      });
      uiBus.publish({
        kind: "state", fp, tabId: req.tabId, state: req,
        _dashboard_id: actor.dashboardId,
      });
      return create(UiReportStateResponseSchema, {});
    },

    async uiListStates(_req, ctx) {
      const actor = requireDashboardActor(ctx.values);
      // Reap inline so a caller polling less often than the interval reaper
      // still never sees a tab that stopped heartbeating > TTL ago.
      reapStaleUiStates(Date.now());
      const entries = [..._uiStatesByTab.values()]
        .filter((entry) => entry.dashboardId === actor.dashboardId);
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
      // A UiCommand with no case set would relay as a no-op every tab
      // silently drops — reject at the wire boundary instead (symmetric to
      // tasksEnqueue's payload validation).
      const command = req.command;
      if (!command?.command.case) {
        throw new ConnectError("uiDispatch requires a command", Code.InvalidArgument);
      }
      await requireDashboardCommandSessions(deps, actor.dashboardId, command);
      // Subscriber count AT publish is restricted to the selected dashboard's
      // live Sync streams. 0 tells a headless caller no one can execute this.
      const delivered = uiBus.subscriberCountFor(actor.dashboardId);
      uiBus.publish({
        kind: "command", targetTabId: req.targetTabId, command,
        _dashboard_id: actor.dashboardId,
      });
      return create(UiDispatchResponseSchema, { delivered });
    },
  };
}
