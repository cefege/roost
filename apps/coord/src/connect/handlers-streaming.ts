// Sync firehose — one server-streaming RPC for in-memory state buses,
// including cell frames, compact terminal-link metadata, and browser UI coordination.

import type { ServiceImpl } from "@connectrpc/connect";
import { ConnectError, Code } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import { CoordinatorService } from "@roost/shared/proto/coordinator_pb";
import {
  FirehoseFrameSchema, type FirehoseFrame, TerminalLinkFrameSchema, SessionPresenceSchema,
  WorkerRoutableFrameSchema, TerminalTitleFrameSchema, LastActivityFrameSchema,
  UiStateFrameSchema, UiCommandFrameSchema, AgentStatusFrameSchema,
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
  permissionBus, mcpBus, terminalLinkBus, globalPresenceBus, auditBus,
  titleBus, lastActivityBus, workerRoutableBus, globalCellBus, agentStatusBus,
  pairBus, uiBus, type TaskBusMsg, type PairRequestDelta, type AuditRow,
} from "../buses.ts";
import { getEventsSince } from "../event-log.ts";
import { listRoutableFps } from "./worker-service.ts";
import { getTitleSnapshot } from "../terminal-title-hub.ts";
import { getUiStateSnapshot } from "./handlers-ui.ts";
import { getLastActivitySnapshot } from "../last-activity-hub.ts";
import { getAgentStatusSnapshot } from "../agent-status-hub.ts";
import { requireAuth } from "./auth-interceptor.ts";
import { isSubscribed } from "./cell-subscriptions.ts";
import { log } from "@roost/shared/log";
import { signal } from "@roost/shared/diag";
import type {
  SessionEvent, WorkspaceDelta, WebhookTokenDelta, PermissionRuleDelta,
  McpStreamMessage, WorkerPresenceEvent, HostMetrics, AgentStatusUpdate,
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
// Ordering contract: subscribe FIRST, emit retained volatile snapshots, flush
// live frames that arrived while a paced seed was in progress, THEN backfill.
// yieldedSessionIds dedups live events whose _event_id was already replayed by
// the backfill. Legacy callers omit pacedSeedPush and retain synchronous seeds.
export interface SyncFeedSeedOptions {
  pacedSeedPush: (frame: FirehoseFrame) => Promise<boolean>;
}

export function startSyncFeed(
  deps: ConnectDeps,
  sinceEventId: number,
  sink: (f: FirehoseFrame) => void,
  /** Per-tab identity of the socket this feed serves. Non-null → the two hot
   *  per-session buses ship only sessions this tab claimed. null (older SPA,
   *  CLI, test client) FAILS OPEN and ships every session, as before. */
  viewerKey: string | null = null,
  seedOptions?: SyncFeedSeedOptions,
): { seeded: Promise<void>; backfill: () => Promise<void>; dispose: () => void } {
  let disposed = false;
  let seeding = seedOptions !== undefined;
  const queuedLiveFrames: FirehoseFrame[] = [];
  const push = (frame: FirehoseFrame): void => {
    if (disposed) return;
    if (seeding) {
      queuedLiveFrames.push(frame);
      return;
    }
    sink(frame);
  };
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
  const agentStatusFrame = (status: AgentStatusUpdate): FirehoseFrame =>
    create(FirehoseFrameSchema, {
      frame: {
        case: "agentStatus",
        value: create(AgentStatusFrameSchema, {
          sessionId: status.session_id,
          agentId: status.agent_id,
          state: status.state,
          message: status.message,
          revision: BigInt(status.revision),
          completedRevision: BigInt(status.completed_revision),
          updatedAt: status.updated_at,
          active: status.active,
        }),
      },
    });

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
    // Compact mappings are deliberately unfiltered: a pane may be offscreen
    // when its link arrives, and the browser registry retains it until revisit.
    terminalLinkBus.subscribe(({ session_id, text, uri }) => {
      push(create(FirehoseFrameSchema, {
        frame: { case: "terminalLink", value: create(TerminalLinkFrameSchema, {
          sessionId: session_id, text, uri,
        })},
      }));
    }),
    globalPresenceBus.subscribe(({ session_id, data }) => {
      if (viewerKey !== null && typeof data === "object" && data !== null) {
        const payload = data as { kind?: unknown; viewer_id?: unknown };
        if (
          (payload.kind === "presence-delta" || payload.kind === "presence-leave")
          && payload.viewer_id === viewerKey
        ) return;
      }
      push(create(FirehoseFrameSchema, {
        frame: { case: "sessionPresence", value: create(SessionPresenceSchema, {
          sessionId: session_id, payloadJson: JSON.stringify(data),
        })},
      }));
    }),
    // R11 cell-grid cell-shipping. Bus payload is already a PbCellGridFrame
    // (session_id stamped by byte-hub::publishCellGrid).
    globalCellBus.subscribe((frame) => {
      if (viewerKey && !isSubscribed(viewerKey, frame.sessionId)) return;
      frame.coordFanoutMs = BigInt(Date.now());
      push(create(FirehoseFrameSchema, { frame: { case: "cellGrid", value: frame } }));
    }),
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
    agentStatusBus.subscribe((status) => push(agentStatusFrame(status))),
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

  function* retainedSeedFrames(): Generator<FirehoseFrame> {
    // Live routable worker membership is volatile, so seed it before the
    // per-session snapshots below.
    yield create(FirehoseFrameSchema, {
      frame: {
        case: "workerRoutable",
        value: create(WorkerRoutableFrameSchema, { fps: listRoutableFps() }),
      },
    });

    for (const { session_id, title } of getTitleSnapshot()) {
      yield create(FirehoseFrameSchema, {
        frame: {
          case: "terminalTitle",
          value: create(TerminalTitleFrameSchema, { sessionId: session_id, title }),
        },
      });
    }

    for (const { session_id, ts_ms } of getLastActivitySnapshot()) {
      yield create(FirehoseFrameSchema, {
        frame: {
          case: "lastActivity",
          value: create(LastActivityFrameSchema, { sessionId: session_id, tsMs: ts_ms }),
        },
      });
    }

    for (const status of getAgentStatusSnapshot()) yield agentStatusFrame(status);

    for (const { fp, tabId, state } of getUiStateSnapshot()) {
      yield create(FirehoseFrameSchema, {
        frame: {
          case: "uiState",
          value: create(UiStateFrameSchema, { fp, tabId, state }),
        },
      });
    }
  }

  const loadPairSnapshot = async (): Promise<FirehoseFrame | null> => {
    try {
      const pending = await deps.db.selectFrom("pair_requests")
        .select(["ephemeral_id", "label", "created_at_ms"])
        .where("status", "=", "pending").execute();
      return create(FirehoseFrameSchema, {
        frame: {
          case: "pairRequestDelta",
          value: create(PairRequestDeltaProtoSchema, {
            kind: {
              case: "snapshot",
              value: create(PairRequestsSnapshotSchema, {
                pending: pending.map((row) => create(PairRequestSchema, {
                  ephemeralId: row.ephemeral_id,
                  label: row.label,
                  createdAtMs: BigInt(row.created_at_ms),
                })),
              }),
            },
          }),
        },
      });
    } catch (e) {
      log.warn("connect.sync", "pair_seed_failed", { error: String(e) });
      return null;
    }
  };

  let seeded: Promise<void>;
  if (!seedOptions) {
    // Legacy clients are not application-windowed. Preserve their current
    // synchronous retained burst and floating pair snapshot behavior.
    for (const frame of retainedSeedFrames()) push(frame);
    seeded = Promise.resolve();
    void (async () => {
      const pairSnapshot = await loadPairSnapshot();
      if (pairSnapshot) push(pairSnapshot);
    })();
  } else {
    // Start after startSyncFeed returns so open() can first retain this feed on
    // ws.data. Every await is ACK-coupled by sync-ws-handler; no fixed chunk can
    // outrun a slow consumer. Live frames queue only for this seed phase, then
    // flush in original arrival order before durable replay begins.
    seeded = Promise.resolve().then(async () => {
      for (const frame of retainedSeedFrames()) {
        if (disposed || !(await seedOptions.pacedSeedPush(frame))) return;
      }

      const pairSnapshot = await loadPairSnapshot();
      if (
        pairSnapshot
        && (disposed || !(await seedOptions.pacedSeedPush(pairSnapshot)))
      ) return;

      let queueIndex = 0;
      while (!disposed) {
        if (queueIndex === queuedLiveFrames.length) {
          seeding = false;
          queuedLiveFrames.length = 0;
          return;
        }
        const frame = queuedLiveFrames[queueIndex++]!;
        if (!(await seedOptions.pacedSeedPush(frame))) return;
      }
    }).catch((e) => {
      log.warn("connect.sync", "retained_seed_failed", { error: String(e) });
    });
  }

  // T1.4 — reconnect backfill. Subscribe and finish any paced retained seed
  // first, so nothing can land in the subscription/seed gap. Once seeding ends,
  // live session events and durable rows retain the existing whichever-emits-
  // first event-id dedup while replay yields every sixteen rows.
  const backfill = async (): Promise<void> => {
    if (seedOptions) await seeded;
    if (disposed || sinceEventId <= 0) return;
    try {
      const rows = await getEventsSince(deps.db, sinceEventId, 1000);
      for (let i = 0; i < rows.length; i += 1) {
        const { id, event } = rows[i]!;
        emitSession(event, id);
        if ((i + 1) % 16 === 0) {
          await new Promise<void>((resolve) => setImmediate(resolve));
        }
      }
      if (rows.length === 1000) {
        signal("sync.backfill_truncated", { sinceEventId, returned: rows.length, cooldownKey: "sync" });
      }
    } catch (e) {
      log.warn("connect.sync", "backfill_failed", { error: String(e), sinceEventId });
      signal("sync.backfill_failed", { error: String(e), sinceEventId, cooldownKey: "sync" });
    }
  };

  const dispose = (): void => {
    if (disposed) return;
    disposed = true;
    seeding = false;
    queuedLiveFrames.length = 0;
    for (const unsubscribe of unsubs) unsubscribe();
  };

  return { seeded, backfill, dispose };
}
