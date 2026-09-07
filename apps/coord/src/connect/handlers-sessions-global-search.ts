// Owns coordinator-authorized, dashboard-wide terminal-content search and cancel.
// It enumerates live session authority from SQLite, fans one bounded request to
// each routable worker, validates every worker result, and projects partials.
// Continuation and cancel ordering are delegated to the injected cursor owner.

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  CoordinatorService,
  GlobalSearchPartialReason,
  SessionsSearchGlobalMatchSchema,
  SessionsSearchGlobalPartialSchema,
  SessionsSearchGlobalResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
  GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS,
  GLOBAL_TERMINAL_SEARCH_WORK_DEADLINE_MS,
  TerminalSearchIdSchema,
  TerminalSearchQuerySchema,
} from "@roost/shared/terminal-search";
import { asSessionId } from "@roost/shared/wire";
import {
  cancelPendingRpc,
  createPendingRpc,
  rejectPendingRpcUnavailable,
} from "../router/pending-rpcs.ts";
import {
  requireAccountDevice,
  requireDashboardActor,
  requireSearchTabId,
} from "./auth-interceptor.ts";
import { sendBrowserCmd } from "./router-helpers.ts";
import type { ConnectDeps } from "./router.ts";
import {
  groupOnlineGlobalSearchSessions,
  listAuthorizedGlobalSearchSessions,
  outcomeForGlobalSearchEntry,
  reauthorizeGlobalSearchSessions,
  sendGlobalSearchCancellationBatches,
  validateGlobalSearchGroupResult,
  type GlobalSearchSessionOutcome,
} from "./global-search-fanout.ts";
import {
  GlobalSearchCursorOwner,
  type GlobalSearchCursorBinding,
  type GlobalSearchIdentity,
  type GlobalSearchSessionPosition,
} from "./global-search-cursors.ts";
import { makeGlobalSearchCancelHandler } from "./global-search-cancel.ts";
import { GlobalSearchWorkerLaneOwner } from "./global-search-worker-lanes.ts";
import { normalizeGlobalSearchPageLimits } from "./global-search-options.ts";

type GlobalSearchMethods = "sessionsSearchGlobal" | "sessionsCancelGlobalSearch";


export function makeSessionGlobalSearchHandlers(
  deps: ConnectDeps,
  cursorOwner: GlobalSearchCursorOwner,
  workerLanes: GlobalSearchWorkerLaneOwner,
): Pick<ServiceImpl<typeof CoordinatorService>, GlobalSearchMethods> {
  return {
    async sessionsSearchGlobal(req, ctx) {
      const actor = requireDashboardActor(ctx.values);
      const caller = requireAccountDevice(ctx.values);
      if (!TerminalSearchIdSchema.safeParse(req.searchId).success) {
        throw new ConnectError(
          "global search search_id must contain 1 to 64 characters",
          Code.InvalidArgument,
        );
      }
      if (!TerminalSearchQuerySchema.safeParse(req.query).success) {
        throw new ConnectError(
          "global search query must contain at most 256 Unicode code points",
          Code.InvalidArgument,
        );
      }
      const limits = normalizeGlobalSearchPageLimits(req);
      const tabId = requireSearchTabId(ctx.values);
      const pageDeadlineAt = workerLanes.deadlineAfter(
        GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS,
      );
      const viewerId = `${caller.fingerprint}:${tabId}`;
      const identity: GlobalSearchIdentity = {
        dashboardId: actor.dashboardId,
        deviceFingerprint: caller.fingerprint,
        tabId,
        searchId: req.searchId,
      };
      const binding: GlobalSearchCursorBinding = {
        ...identity,
        query: req.query,
        caseSensitive: req.caseSensitive,
        ...limits,
      };
      const admission = cursorOwner.beginSearch(identity);
      if (admission === "capacity") {
        throw new ConnectError("too many active global searches", Code.ResourceExhausted);
      }
      if (admission !== "started") {
        throw new ConnectError("global search cancelled", Code.Canceled);
      }
      const pendingByWorker = new Map<string, string>();
      let pageSessions: readonly GlobalSearchSessionPosition[] = [];
      let authorizedSessions: readonly GlobalSearchSessionPosition[] = [];
      let eligibleSessions = 0;
      const searchedSessionIds = new Set<string>();
      const workAbort = new AbortController();
      const abortSearch = (): void => {
        const cancellation = cursorOwner.prepareCancellation(identity);
        if (!cancellation.shouldDispatch) return;
        try {
          sendGlobalSearchCancellationBatches(
            caller,
            viewerId,
            req.searchId,
            cancellation.selectedSessions,
          );
        } finally {
          cursorOwner.completeCancellation(identity);
        }
      };
      ctx.signal.addEventListener("abort", abortSearch, { once: true });
      let removeOwnerCancellation = () => {};
      try {
        const outcomes = new Map<string, GlobalSearchSessionOutcome>();
        if (req.cursor !== undefined) {
          const cursorProgress = cursorOwner.claimCursor(req.cursor, binding);
          if (!cursorProgress) {
            throw new ConnectError(
              "global search cursor is invalid or expired",
              Code.InvalidArgument,
            );
          }
          pageSessions = cursorProgress.sessions;
          eligibleSessions = cursorProgress.eligibleSessions;
          for (const sessionId of cursorProgress.searchedSessionIds) {
            searchedSessionIds.add(sessionId);
          }
          if (!cursorOwner.selectSessions(identity, pageSessions)) {
            throw new ConnectError("global search cancelled", Code.Canceled);
          }
          const reauthorized = await reauthorizeGlobalSearchSessions(
            deps.db,
            actor.dashboardId,
            pageSessions,
          );
          for (const sessionId of reauthorized.closedSessionIds) {
            outcomes.set(sessionId, {
              matches: [],
              partialReason: GlobalSearchPartialReason.SESSION_CLOSED,
              searched: false,
            });
          }
          authorizedSessions = reauthorized.authorized;
          if (!cursorOwner.selectSessions(identity, authorizedSessions)) {
            throw new ConnectError("global search cancelled", Code.Canceled);
          }
        } else {
          const authorizedPage = await listAuthorizedGlobalSearchSessions(
            deps.db,
            actor.dashboardId,
            limits.maxSessions,
          );
          pageSessions = authorizedPage.sessions;
          eligibleSessions = authorizedPage.eligibleSessions;
          authorizedSessions = pageSessions;
          if (!cursorOwner.selectSessions(identity, authorizedSessions)) {
            throw new ConnectError("global search cancelled", Code.Canceled);
          }
        }
        if (cursorOwner.isCancelled(identity) || ctx.signal.aborted) {
          throw new ConnectError("global search cancelled", Code.Canceled);
        }
        const groups = groupOnlineGlobalSearchSessions(
          authorizedSessions,
          outcomes,
          limits.maxMatches,
        );
        removeOwnerCancellation = cursorOwner.onCancel(identity, () => {
          workAbort.abort();
          for (const [workerFp, requestId] of pendingByWorker) {
            cancelPendingRpc(requestId, workerFp);
          }
        });
        await Promise.all(groups.map(async (group) => {
          const lease = await workerLanes.acquire(
            group.workerFp,
            pageDeadlineAt - GLOBAL_TERMINAL_SEARCH_WORK_DEADLINE_MS,
            workAbort.signal,
          );
          if (!lease) {
            if (!cursorOwner.isCancelled(identity)) {
              for (const session of group.sessions) {
                outcomes.set(session.sessionId, {
                  matches: [],
                  continuation: session,
                  partialReason: GlobalSearchPartialReason.DEADLINE,
                  searched: false,
                });
              }
            }
            return;
          }
          try {
            if (cursorOwner.isCancelled(identity)) return;
            const remainingMs = workerLanes.remainingMs(pageDeadlineAt);
            if (remainingMs < GLOBAL_TERMINAL_SEARCH_WORK_DEADLINE_MS) {
              for (const session of group.sessions) {
                outcomes.set(session.sessionId, {
                  matches: [],
                  continuation: session,
                  partialReason: GlobalSearchPartialReason.DEADLINE,
                  searched: false,
                });
              }
              return;
            }
            const pending = createPendingRpc<unknown>(
              Math.max(1, Math.ceil(remainingMs)),
              group.workerFp,
            );
            pendingByWorker.set(group.workerFp, pending.request_id);
            try {
              sendBrowserCmd(group.socket, caller, pending.request_id, {
                kind: "search-scrollback-batch",
                request_id: pending.request_id,
                search_id: req.searchId,
                query: req.query,
                case_sensitive: req.caseSensitive,
                sessions: group.sessions.map((session) => ({
                  session_id: asSessionId(session.sessionId),
                  grid_epoch: session.gridEpoch,
                  ...(session.beforeRow === undefined
                    ? {}
                    : { before_row: session.beforeRow }),
                })),
                max_rows_per_session: limits.maxRowsPerSession,
                max_matches: group.matchBudget,
                deadline_ms: GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS,
              }, viewerId);
            } catch (error) {
              rejectPendingRpcUnavailable(
                pending.request_id,
                `global search send failed: ${String(error)}`,
                group.workerFp,
              );
            }
            try {
              const rawResult = await pending.promise;
              const entries = validateGlobalSearchGroupResult(rawResult, group, limits);
              if (!entries) {
                for (const session of group.sessions) {
                  outcomes.set(session.sessionId, {
                    matches: [],
                    continuation: session,
                    partialReason: GlobalSearchPartialReason.MALFORMED_RESULT,
                    searched: false,
                  });
                }
                return;
              }
              for (let index = 0; index < entries.length; index++) {
                const entry = entries[index]!;
                const session = group.sessions[index]!;
                outcomes.set(
                  entry.session_id,
                  outcomeForGlobalSearchEntry(session, entry),
                );
              }
            } catch (error) {
              if (cursorOwner.isCancelled(identity)) return;
              const reason = error instanceof ConnectError
                  && error.code === Code.DeadlineExceeded
                ? GlobalSearchPartialReason.DEADLINE
                : error instanceof ConnectError && error.code === Code.Unavailable
                  ? GlobalSearchPartialReason.WORKER_UNAVAILABLE
                  : GlobalSearchPartialReason.MALFORMED_RESULT;
              for (const session of group.sessions) {
                outcomes.set(session.sessionId, {
                  matches: [],
                  continuation: session,
                  partialReason: reason,
                  searched: false,
                });
              }
            } finally {
              pendingByWorker.delete(group.workerFp);
            }
          } finally {
            lease.release();
          }
        }));
        if (cursorOwner.isCancelled(identity) || ctx.signal.aborted) {
          throw new ConnectError("global search cancelled", Code.Canceled);
        }
        const orderedOutcomes = pageSessions.map((session) => ({
          session,
          outcome: outcomes.get(session.sessionId) ?? {
            matches: [],
            partialReason: GlobalSearchPartialReason.MALFORMED_RESULT,
            searched: false,
          },
        }));
        for (const { session, outcome } of orderedOutcomes) {
          if (outcome.searched) searchedSessionIds.add(session.sessionId);
        }
        const continuations = orderedOutcomes
          .filter(({ outcome }) => outcome.continuation !== undefined)
          .sort((left, right) => Number(left.outcome.searched) - Number(right.outcome.searched))
          .map(({ session, outcome }) => ({
            position: outcome.continuation!,
            searched: outcome.searched,
            requestedBeforeRow: session.beforeRow,
          }));
        const nextCursor = continuations.length > 0
          ? cursorOwner.issueCursor({
              binding,
              continuations,
              eligibleSessions,
              searchedSessionIds: [...searchedSessionIds],
            })
          : undefined;
        const partials = orderedOutcomes.flatMap(({ session, outcome }) => {
          if (outcome.partialReason === undefined) return [];
          return [create(SessionsSearchGlobalPartialSchema, {
            sessionId: session.sessionId,
            reason: outcome.partialReason,
          })];
        });
        return create(SessionsSearchGlobalResponseSchema, {
          matches: orderedOutcomes.flatMap(({ outcome }) =>
            outcome.matches.map((match) =>
              create(SessionsSearchGlobalMatchSchema, {
                sessionId: match.sessionId,
                row: BigInt(match.row),
                col: match.col,
                len: match.len,
                preview: match.preview,
                gridEpoch: match.gridEpoch,
              })
            )
          ),
          partials,
          nextCursor,
          searchedSessions: searchedSessionIds.size,
          eligibleSessions,
          // Sessions past the page cap were never touched, so a page that
          // searched fewer than the eligible count is truncated even when
          // nothing failed and no continuation remains.
          truncated: nextCursor !== undefined
            || partials.length > 0
            || searchedSessionIds.size < eligibleSessions,
        });
      } finally {
        removeOwnerCancellation();
        ctx.signal.removeEventListener("abort", abortSearch);
        for (const [workerFp, requestId] of pendingByWorker) {
          cancelPendingRpc(requestId, workerFp);
        }
        cursorOwner.finishSearch(identity);
      }
    },

    sessionsCancelGlobalSearch: makeGlobalSearchCancelHandler(deps, cursorOwner),
  };
}
