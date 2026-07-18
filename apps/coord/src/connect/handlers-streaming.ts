// Sync firehose — the one server-streaming RPC. Multiplexes the in-memory
// buses (sessions / presence / workspace / task / permission / mcp / webhook /
// audit / bytes / session-presence / cell / claude-status / worker-routable /
// pair / ui) onto a single FirehoseFrame stream, with reconnect backfill of
// the durable sessionBus via sinceEventId. Each bus payload is adapted to its
// proto frame shape. Spread into router.ts's single
// router.service() literal. Split out of router.ts (400-line cap); kept apart
// from the unary domains because it owns all the bus wiring + frame adapters.

import type { ServiceImpl } from "@connectrpc/connect";
import { ConnectError, Code } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import { CoordinatorService } from "@roost/shared/proto/coordinator_pb";
import {
  FirehoseFrameSchema, type FirehoseFrame, BytesFrameSchema, SessionPresenceSchema,
  ClaudeStatusFrameSchema, WorkerRoutableFrameSchema, TerminalTitleFrameSchema,
  LastActivityFrameSchema, UiStateFrameSchema, UiCommandFrameSchema,
} from "@roost/shared/proto/sync_pb";
import { eventToProto } from "@roost/shared/wire/event-proto";
import {
  WorkspaceDeltaProtoSchema, WorkspaceSessionsSetSchema,
  TaskDeltaProtoSchema, WebhookTokenDeltaProtoSchema,
  PermissionRuleDeltaProtoSchema, McpStreamMessageProtoSchema,
  McpRelayEventSchema, WorkerPresenceProtoSchema, WorkerHeartbeatSchema,
  PairRequestDeltaProtoSchema, PairRequestsSnapshotSchema,
} from "@roost/shared/proto/events_pb";
import {
  WorkspaceSchema as WorkspacePbSchema,
  WebhookTokenSchema as WebhookTokenPbSchema,
  PermissionRuleSchema as PermissionRulePbSchema,
  McpRelaySchema as McpRelayPbSchema,
  WorkerSchema as WorkerPbSchema,
  HostMetricsSchema as HostMetricsPbSchema,
  AuditRowSchema, PairRequestSchema,
} from "@roost/shared/proto/wire_pb";
import {
  sessionBus, presenceBus, workspaceBus, taskBus, webhookBus,
  permissionBus, mcpBus, globalBytesBus, globalPresenceBus, auditBus,
  claudeStatusBus, titleBus, lastActivityBus, workerRoutableBus, globalCellBus,
  pairBus, uiBus, type TaskBusMsg, type PairRequestDelta, type AuditRow,
} from "../buses.ts";
import { getEventsSince } from "../event-log.ts";
import { listRoutableFps } from "./worker-service.ts";
import { getTitleSnapshot } from "../terminal-title-hub.ts";
import { getUiStateSnapshot } from "./handlers-ui.ts";
import { getLastActivitySnapshot } from "../last-activity-hub.ts";
import { snapshotClaudeStatuses } from "../byte-hub.ts";
import { requireAuth } from "./auth-interceptor.ts";
import { log } from "@roost/shared/log";
import { signal } from "@roost/shared/diag";
import type {
  SessionEvent, WorkspaceDelta, WebhookTokenDelta, PermissionRuleDelta,
  McpStreamMessage, WorkerPresenceEvent, HostMetrics,
} from "@roost/shared/wire";
import type { ConnectDeps } from "./router.ts";

type StreamingMethods = "sync";

export function makeStreamingHandlers(
  _deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, StreamingMethods> {
  return {
    // The Sync firehose moved to a raw Bun WebSocket at /ws/coord-sync
    // (sync-ws-handler.ts) to dodge the Bun 1.3.14 use-after-free in
    // RequestContext.onAbort that crashed coord whenever a browser aborted
    // this long-lived streaming response. The feed lives in startSyncFeed
    // below, now consumed ONLY by the WS handler; this Connect method stays
    // declared for ServiceImpl completeness but is unimplemented so the
    // crashing abort-listener path is GONE, not merely unused.
    async *sync(_req, ctx): AsyncGenerator<FirehoseFrame> {
      requireAuth(ctx.values);
      throw new ConnectError("sync moved to /ws/coord-sync", Code.Unimplemented);
    },
  };
}

// startSyncFeed — the shared Sync firehose engine. Multiplexes every in-memory
// bus onto FirehoseFrames, seeds the volatile per-connection snapshots, and
// (via backfill) replays the durable sessionBus from sinceEventId, funnelling
// each frame into the single `push` sink. Consumed by the raw-WS handler
// (sync-ws-handler.ts). SHARED so the WS reader can't diverge from the frame
// shapes the SPA already decodes.
//
// Ordering contract: subscribe FIRST (unsubs below), THEN call backfill() — a
// sessionBus.publish landing between getEventsSince and the subscribe would
// otherwise be lost on this connection. yieldedSessionIds dedups live events
// whose _event_id was already replayed by the backfill.
export function startSyncFeed(
  deps: ConnectDeps,
  sinceEventId: number,
  push: (f: FirehoseFrame) => void,
): { backfill: () => Promise<void>; dispose: () => void } {
  // Backfill + live sessionBus both encode SessionEvent through here.
  const sessionFirehoseFrame = (e: SessionEvent, eventId: number): FirehoseFrame =>
    create(FirehoseFrameSchema, { frame: { case: "sessionEvent", value: eventToProto(e, eventId) } });

  // T1.2 part 2 — typed delta adapters. Each returns a proto-shaped
  // FirehoseFrame when the bus payload matches a known shape, else
  // null so the caller falls back to the JsonEvent path.
  const workspaceFrame = (e: WorkspaceDelta): FirehoseFrame | null => {
    if (e.kind === "created" || e.kind === "updated") {
      return create(FirehoseFrameSchema, { frame: { case: "workspaceDelta", value: create(WorkspaceDeltaProtoSchema, {
        kind: { case: e.kind, value: create(WorkspacePbSchema, {
          id: e.workspace.id, workerFp: e.workspace.worker_fp,
          name: e.workspace.name, folderPath: e.workspace.folder_path,
          color: e.workspace.color ?? undefined, position: e.workspace.position,
          version: BigInt(e.workspace.version),
          createdAtMs: BigInt(e.workspace.created_at_ms),
          updatedAtMs: BigInt(e.workspace.updated_at_ms),
          sessionIds: e.workspace.session_ids,
        }) },
      })}});
    }
    if (e.kind === "deleted") {
      return create(FirehoseFrameSchema, { frame: { case: "workspaceDelta", value: create(WorkspaceDeltaProtoSchema, {
        kind: { case: "deletedId", value: e.id },
      })}});
    }
    if (e.kind === "sessions-set") {
      return create(FirehoseFrameSchema, { frame: { case: "workspaceDelta", value: create(WorkspaceDeltaProtoSchema, {
        kind: { case: "sessionsSet", value: create(WorkspaceSessionsSetSchema, {
          workspaceId: e.id, sessionIds: e.session_ids, version: BigInt(e.version),
        }) },
      })}});
    }
    return null;
  };
  const taskFrame = (e: TaskBusMsg): FirehoseFrame =>
    create(FirehoseFrameSchema, { frame: { case: "taskDelta", value: create(TaskDeltaProtoSchema, {
      kind: { case: e.kind, value: e.task },
    })}});
  const webhookFrame = (e: WebhookTokenDelta): FirehoseFrame | null => {
    if (e.kind === "created") {
      return create(FirehoseFrameSchema, { frame: { case: "webhookTokenDelta", value: create(WebhookTokenDeltaProtoSchema, {
        kind: { case: "created", value: create(WebhookTokenPbSchema, {
          id: e.token.id, label: e.token.label, last4: e.token.last4, scopes: e.token.scopes,
          createdAtMs: BigInt(e.token.created_at_ms),
          lastUsedAtMs: e.token.last_used_at_ms != null ? BigInt(e.token.last_used_at_ms) : undefined,
        }) },
      })}});
    }
    if (e.kind === "deleted") {
      return create(FirehoseFrameSchema, { frame: { case: "webhookTokenDelta", value: create(WebhookTokenDeltaProtoSchema, {
        kind: { case: "deletedId", value: e.id },
      })}});
    }
    return null;
  };
  const permFrame = (e: PermissionRuleDelta): FirehoseFrame | null => {
    if (e.kind === "created" || e.kind === "updated") {
      return create(FirehoseFrameSchema, { frame: { case: "permissionDelta", value: create(PermissionRuleDeltaProtoSchema, {
        kind: { case: e.kind, value: create(PermissionRulePbSchema, {
          id: e.rule.id, toolPattern: e.rule.tool_pattern, folderGlob: e.rule.folder_glob,
          decision: e.rule.decision, enabled: e.rule.enabled,
          createdAtMs: BigInt(e.rule.created_at_ms),
        }) },
      })}});
    }
    if (e.kind === "deleted") {
      return create(FirehoseFrameSchema, { frame: { case: "permissionDelta", value: create(PermissionRuleDeltaProtoSchema, {
        kind: { case: "deletedId", value: e.id },
      })}});
    }
    return null;
  };
  const mcpFrame = (e: McpStreamMessage): FirehoseFrame | null => {
    if ("kind" in e && (e.kind === "created" || e.kind === "updated")) {
      return create(FirehoseFrameSchema, { frame: { case: "mcpMsg", value: create(McpStreamMessageProtoSchema, {
        kind: { case: e.kind, value: create(McpRelayPbSchema, {
          id: e.relay.id, label: e.relay.label, kind: e.relay.kind,
          configJson: JSON.stringify(e.relay.config),
          createdAtMs: BigInt(e.relay.created_at_ms),
        }) },
      })}});
    }
    if ("kind" in e && e.kind === "deleted") {
      return create(FirehoseFrameSchema, { frame: { case: "mcpMsg", value: create(McpStreamMessageProtoSchema, {
        kind: { case: "deletedId", value: e.id },
      })}});
    }
    if ("relay_id" in e && "payload" in e) {
      return create(FirehoseFrameSchema, { frame: { case: "mcpMsg", value: create(McpStreamMessageProtoSchema, {
        kind: { case: "event", value: create(McpRelayEventSchema, {
          relayId: e.relay_id, payloadJson: JSON.stringify(e.payload), ts: BigInt(e.ts),
        }) },
      })}});
    }
    return null;
  };
  const presenceFrame = (e: WorkerPresenceEvent): FirehoseFrame | null => {
    const hm = (m: HostMetrics) => create(HostMetricsPbSchema, {
      cpuPct: m.cpu_pct,
      memUsedBytes: BigInt(m.mem_used_bytes), memTotalBytes: BigInt(m.mem_total_bytes),
      diskUsedBytes: BigInt(m.disk_used_bytes), diskTotalBytes: BigInt(m.disk_total_bytes),
      netRxBps: BigInt(m.net_rx_bps), netTxBps: BigInt(m.net_tx_bps),
      sampledAtMs: BigInt(m.sampled_at_ms),
    });
    if (e.kind === "registered") {
      return create(FirehoseFrameSchema, { frame: { case: "workerPresence", value: create(WorkerPresenceProtoSchema, {
        kind: { case: "registered", value: create(WorkerPbSchema, {
          fp: e.worker.fp, label: e.worker.label, os: e.worker.os,
          gitSha: e.worker.git_sha ?? undefined,
          hostMetrics: e.worker.host_metrics ? hm(e.worker.host_metrics) : undefined,
          registeredAtMs: BigInt(e.worker.registered_at_ms),
          lastSeenMs: BigInt(e.worker.last_seen_ms),
        }) },
      })}});
    }
    if (e.kind === "heartbeat") {
      return create(FirehoseFrameSchema, { frame: { case: "workerPresence", value: create(WorkerPresenceProtoSchema, {
        kind: { case: "heartbeat", value: create(WorkerHeartbeatSchema, {
          workerFp: e.fp, lastSeenMs: BigInt(e.last_seen_ms),
          hostMetrics: e.host_metrics ? hm(e.host_metrics) : undefined,
        }) },
      })}});
    }
    if (e.kind === "removed") {
      return create(FirehoseFrameSchema, { frame: { case: "workerPresence", value: create(WorkerPresenceProtoSchema, {
        kind: { case: "removedFp", value: e.fp },
      })}});
    }
    return null;
  };
  const auditFrame = (e: AuditRow): FirehoseFrame =>
    create(FirehoseFrameSchema, { frame: { case: "auditRow", value: create(AuditRowSchema, {
      id: BigInt(e.id), ts: BigInt(e.ts),
      callerFp: e.caller_fp ?? undefined,
      callerLabel: e.caller_label ?? undefined,
      method: e.method, path: e.path, status: e.status,
      traceId: e.trace_id ?? undefined,
    })}});
  // Pair-request deltas (perf sweep C2.4 — replaces the SPA pairList
  // poller). Bus payload is the coord-internal PairRequestDelta shape.
  const pairFrame = (e: PairRequestDelta): FirehoseFrame =>
    e.kind === "pending"
      ? create(FirehoseFrameSchema, { frame: { case: "pairRequestDelta", value: create(PairRequestDeltaProtoSchema, {
          kind: { case: "pending", value: create(PairRequestSchema, {
            ephemeralId: e.ephemeral_id, label: e.label, createdAtMs: BigInt(e.created_at_ms),
          }) },
        })}})
      : create(FirehoseFrameSchema, { frame: { case: "pairRequestDelta", value: create(PairRequestDeltaProtoSchema, {
          kind: { case: "removedId", value: e.ephemeral_id },
        })}});

  // B10 — KNOWN INVARIANT (deliberate, not a bug): only sessionBus is
  // durable. sessionBus events live in the `events` table, so a reconnect
  // backfills them via sinceEventId (below). The OTHER buses (presence /
  // workspace / task / permission / mcp / webhook / audit) are
  // fire-and-forget in-memory — a delta emitted while a SPA is
  // disconnected is NOT replayed on reconnect. That gap is closed two
  // other ways, by design: (1) the SPA re-bootstraps every domain via the
  // unary *List calls on reload/visibility-regain, and (2) every mutation
  // republishes full state on each UPDATE (the publishTaskState-on-every-
  // UPDATE rule, feedback_task_state_delta_only_created). Do NOT add a
  // per-bus cursor / durable-bus recovery layer for this — it's a large
  // build for a gap the bootstrap already covers (audit wf_728b67c1 B10,
  // deferred). If a specific domain proves to need durable replay, give
  // THAT domain an events-table projection like sessions, don't build a
  // generic layer.
  const yieldedSessionIds = new Set<number>();
  // Dedup session events across backfill + live delivery: whichever path emits
  // a given _event_id FIRST records it; the other skips. The check-add-push is
  // synchronous (atomic in single-threaded JS), so a live event firing during
  // the backfill await can't double-send even though the sink (ws.send) is
  // unbuffered — this replaces the old "queue drained only after backfill" order.
  const emitSession = (e: SessionEvent, eventId: number): void => {
    if (eventId > 0) {
      if (yieldedSessionIds.has(eventId)) return;
      yieldedSessionIds.add(eventId);
    }
    push(sessionFirehoseFrame(e, eventId));
  };

  const unsubs = [
    sessionBus.subscribe(e => {
      // _event_id is stamped onto the payload by event-log.ts after the
      // durable insert; it is not part of the SessionEvent wire type.
      const stamped = e as SessionEvent & { _event_id?: number };
      const eid = stamped._event_id ?? 0;
      emitSession(e, eid);
    }),
    presenceBus.subscribe(e => { const f = presenceFrame(e); if (f) push(f); }),
    workspaceBus.subscribe(e => { const f = workspaceFrame(e); if (f) push(f); }),
    taskBus.subscribe(e => push(taskFrame(e))),
    permissionBus.subscribe(e => { const f = permFrame(e); if (f) push(f); }),
    mcpBus.subscribe(e => { const f = mcpFrame(e); if (f) push(f); }),
    webhookBus.subscribe(e => { const f = webhookFrame(e); if (f) push(f); }),
    auditBus.subscribe(e => push(auditFrame(e))),
    pairBus.subscribe(e => push(pairFrame(e))),
    globalBytesBus.subscribe(({ session_id, bytes }) =>
      push(create(FirehoseFrameSchema, {
        frame: { case: "bytes", value: create(BytesFrameSchema, {
          sessionId: session_id, data: bytes,
        })},
      }))),
    globalPresenceBus.subscribe(({ session_id, data }) =>
      push(create(FirehoseFrameSchema, {
        frame: { case: "sessionPresence", value: create(SessionPresenceSchema, {
          sessionId: session_id, payloadJson: JSON.stringify(data),
        })},
      }))),
    // R11 cell-grid cell-shipping. Bus payload is already a PbCellGridFrame
    // (session_id stamped by byte-hub::publishCellGrid).
    globalCellBus.subscribe((frame) =>
      push(create(FirehoseFrameSchema, { frame: { case: "cellGrid", value: frame } }))),
    claudeStatusBus.subscribe(({ session_id, status }) =>
      push(create(FirehoseFrameSchema, {
        frame: { case: "claudeStatus", value: create(ClaudeStatusFrameSchema, {
          sessionId: session_id, status,
        })},
      }))),
    titleBus.subscribe(({ session_id, title }) =>
      push(create(FirehoseFrameSchema, {
        frame: { case: "terminalTitle", value: create(TerminalTitleFrameSchema, {
          sessionId: session_id, title,
        })},
      }))),
    lastActivityBus.subscribe(({ session_id, ts_ms }) =>
      push(create(FirehoseFrameSchema, {
        frame: { case: "lastActivity", value: create(LastActivityFrameSchema, {
          sessionId: session_id, tsMs: ts_ms,
        })},
      }))),
    workerRoutableBus.subscribe(({ fps }) =>
      push(create(FirehoseFrameSchema, {
        frame: { case: "workerRoutable", value: create(WorkerRoutableFrameSchema, { fps })},
      }))),
    // ui-cc — both uiBus kinds map 1:1 onto their frames. state = a tab's
    // report re-broadcast (agents watch the spatial model live); command =
    // fire-and-forget UiDispatch relay the target tab executes.
    uiBus.subscribe((m) =>
      push(m.kind === "state"
        ? create(FirehoseFrameSchema, {
            frame: { case: "uiState", value: create(UiStateFrameSchema, {
              fp: m.fp, tabId: m.tabId, state: m.state,
            })},
          })
        : create(FirehoseFrameSchema, {
            frame: { case: "uiCommand", value: create(UiCommandFrameSchema, {
              targetTabId: m.targetTabId, command: m.command,
            })},
          }))),
  ];

  // Seed this connection with the live routable worker set (coord's WS
  // membership) so the online indicator is correct immediately, not after
  // the next connect/disconnect or workersList refresh.
  push(create(FirehoseFrameSchema, {
    frame: { case: "workerRoutable", value: create(WorkerRoutableFrameSchema, { fps: listRoutableFps() })},
  }));

  // Seed the CURRENT terminal title per session (titleBus is volatile too).
  for (const { session_id, title } of getTitleSnapshot()) {
    push(create(FirehoseFrameSchema, {
      frame: { case: "terminalTitle", value: create(TerminalTitleFrameSchema, {
        sessionId: session_id, title,
      })},
    }));
  }

  // Seed the CURRENT claude_status per session (claudeStatusBus is volatile
  // live-delta — B10). Without this a browser connecting AFTER a claude
  // session's last running/idle transition never learns it's a claude and
  // renders it as a plain terminal. isClaudeSession reads claude_status.
  for (const { session_id, status } of snapshotClaudeStatuses()) {
    push(create(FirehoseFrameSchema, {
      frame: { case: "claudeStatus", value: create(ClaudeStatusFrameSchema, {
        sessionId: session_id, status,
      })},
    }));
  }

  // Seed the CURRENT last-activity ms per session so the "Last activity"
  // filter can age out idle open sessions immediately on page load
  // (lastActivityBus is throttled/volatile, not backfilled).
  for (const { session_id, ts_ms } of getLastActivitySnapshot()) {
    push(create(FirehoseFrameSchema, {
      frame: { case: "lastActivity", value: create(LastActivityFrameSchema, {
        sessionId: session_id, tsMs: ts_ms,
      })},
    }));
  }

  // Seed the CURRENT ui_state per live browser tab (uiBus is volatile,
  // presence-class — B10). A fresh Sync subscriber (agent) sees the
  // spatial model immediately instead of waiting on each tab's next
  // heartbeat. ui_command is fire-and-forget by design: never seeded.
  for (const { fp, tabId, state } of getUiStateSnapshot()) {
    push(create(FirehoseFrameSchema, {
      frame: { case: "uiState", value: create(UiStateFrameSchema, { fp, tabId, state }) },
    }));
  }

  // Seed the CURRENT pending pair requests as a FULL snapshot (pairBus is
  // volatile — B10). A snapshot (not per-row deltas) lets the SPA REPLACE
  // its set on every (re)connect, so removals that happened while this
  // browser was disconnected can't linger until the next approve/deny.
  // Subscribes above already ran: a pairBus publish racing this query is
  // ordered delta-then-snapshot, and the snapshot already contains the row
  // (the DB write precedes the publish in the pair handlers). Fired as a
  // floating query (startSyncFeed is sync); dispose() may win the race, but
  // push()'s own send-guard tolerates a closed socket.
  void (async () => {
    try {
      const pending = await deps.db.selectFrom("pair_requests")
        .select(["ephemeral_id", "label", "created_at_ms"])
        .where("status", "=", "pending").execute();
      push(create(FirehoseFrameSchema, { frame: { case: "pairRequestDelta", value: create(PairRequestDeltaProtoSchema, {
        kind: { case: "snapshot", value: create(PairRequestsSnapshotSchema, {
          pending: pending.map((r) => create(PairRequestSchema, {
            ephemeralId: r.ephemeral_id, label: r.label, createdAtMs: BigInt(r.created_at_ms),
          })),
        }) },
      })}}));
    } catch (e) {
      log.warn("connect.sync", "pair_seed_failed", { error: String(e) });
    }
  })();

  // T1.4 — reconnect backfill. SPA tracks the last event id it saw (stamped
  // into the SessionEvent payload as _event_id) and passes it on reopen.
  // Replay capped at 1000 rows so a multi-hour gap doesn't stall forever.
  // Call AFTER subscribes are live so an event firing during the
  // getEventsSince await isn't lost; live sessionBus events whose _event_id
  // matches a backfilled id are deduped via skipLiveSession.
  const backfill = async (): Promise<void> => {
    if (sinceEventId <= 0) return;
    try {
      const rows = await getEventsSince(deps.db, sinceEventId, 1000);
      for (const { id, event } of rows) {
        emitSession(event, id);
      }
      if (rows.length === 1000) {
        signal("sync.backfill_truncated", { sinceEventId, returned: rows.length, cooldownKey: "sync" });
      }
    } catch (e) {
      log.warn("connect.sync", "backfill_failed", { error: String(e), sinceEventId });
      signal("sync.backfill_failed", { error: String(e), sinceEventId, cooldownKey: "sync" });
    }
  };

  const dispose = (): void => { for (const u of unsubs) u(); };

  return { backfill, dispose };
}
