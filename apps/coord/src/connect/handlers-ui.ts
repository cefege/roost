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
// Module-level singleton state + TTL reap, same shape as viewer-tracker.ts:
// ephemeral presence-class data, in-memory map, NOT a DB table. TTL (5 min)
// is generous vs the SPA's 60s heartbeat so live tabs never expire; a closed
// tab simply stops reporting and ages out. Spread into router.ts's single
// router.service() literal. Split per the 400-line handler-file cap.

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  CoordinatorService,
  UiReportStateResponseSchema, UiListStatesResponseSchema,
  UiTabStateSchema, UiDispatchResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import type { UiReportStateRequest } from "@roost/shared/proto/sync_pb";
import { uiBus } from "../buses.ts";
import { requireAuth } from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";

export const UI_STATE_TTL_MS = 5 * 60_000;
const UI_REAP_INTERVAL_MS = 60_000;

interface UiTabEntry {
  fp: string;
  tabId: string;
  lastMs: number;
  state: UiReportStateRequest;
}

// Keyed `${fp}:${tabId}` — the tab id (sessionStorage roost.tabId) alone
// isn't unique across browsers. Underscore export mirrors viewer-tracker's
// _viewersBySession: tests + diag may read/manipulate, handlers own it.
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

/** Current live tab states for Sync-connect seeding (handlers-streaming.ts).
 *  Skips stale entries the interval reaper hasn't swept yet. */
export function getUiStateSnapshot(): Array<{ fp: string; tabId: string; state: UiReportStateRequest }> {
  const now = Date.now();
  const out: Array<{ fp: string; tabId: string; state: UiReportStateRequest }> = [];
  for (const e of _uiStatesByTab.values()) {
    if (now - e.lastMs > UI_STATE_TTL_MS) continue;
    out.push({ fp: e.fp, tabId: e.tabId, state: e.state });
  }
  return out;
}

type UiMethods = "uiReportState" | "uiListStates" | "uiDispatch";
export type UiHandlers = Pick<ServiceImpl<typeof CoordinatorService>, UiMethods>;

export function makeUiHandlers(deps: ConnectDeps): UiHandlers {
  return {
    async uiReportState(req, ctx) {
      // fp comes from the VERIFIED JWT caller (auth interceptor), never from
      // the request body — a tab can't impersonate another browser's state.
      const caller = requireAuth(ctx.values);
      const fp = caller.fingerprint;
      _uiStatesByTab.set(`${fp}:${req.tabId}`, {
        fp, tabId: req.tabId, lastMs: Date.now(), state: req,
      });
      uiBus.publish({ kind: "state", fp, tabId: req.tabId, state: req });
      return create(UiReportStateResponseSchema, {});
    },

    async uiListStates(_req, ctx) {
      requireAuth(ctx.values);
      // Reap inline so a caller polling less often than the interval reaper
      // still never sees a tab that stopped heartbeating > TTL ago.
      reapStaleUiStates(Date.now());
      const entries = [..._uiStatesByTab.values()];
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
      requireAuth(ctx.values);
      // A UiCommand with no case set would relay as a no-op every tab
      // silently drops — reject at the wire boundary instead (symmetric to
      // tasksEnqueue's payload validation).
      const command = req.command;
      if (!command?.command.case) {
        throw new ConnectError("uiDispatch requires a command", Code.InvalidArgument);
      }
      // Subscriber count AT publish ≈ open Sync streams (browsers + any
      // agent watchers). 0 tells a headless caller no one can execute this.
      const delivered = uiBus.subscriberCount;
      uiBus.publish({ kind: "command", targetTabId: req.targetTabId, command });
      return create(UiDispatchResponseSchema, { delivered });
    },
  };
}
