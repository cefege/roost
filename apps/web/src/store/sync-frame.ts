// Per-frame firehose dispatch, split out of store/sync.ts: one switch over every
// wire-frame kind coord can deliver, plus the last-seen event id the reconnect
// backfill resumes from (the two are coupled — only the sessions/sessionEvent
// cases advance the watermark, and they do it inside the same batch).
//
// The transport hands EVERY frame here: v1 sequenced delivery, v2 application
// frames, and the four v2 controls that carry no domain. Nothing in this module
// touches the socket, the handshake, or the redial ladder.

import { batch } from "solid-js";
import { reconcile } from "solid-js/store";
import { signal, diag } from "@roost/shared/diag";
import type { PbCellGridChunk, PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import type { PairRequestDeltaProto } from "@roost/shared/proto/events_pb";
import type {
  AgentStatusFrame,
  FirehoseFrame,
  TerminalViewStateFrame,
  UiCommandFrame,
} from "@roost/shared/proto/sync_pb";
import { protoToEvent } from "@roost/shared/wire/event-proto";
import { relocateBrowserToCoordinator } from "../auth/coordinator-relocation.ts";
import { _dispatchUiCommand } from "../lib/uiCommandDispatch.ts";
import { applyAgentStatusFrame } from "./agent-status.ts";
import { setRootStore, rootStore } from "./root.ts";
import type { PairRequest } from "./root.ts";
import { _dispatchPresence } from "./sync-dispatch.ts";
import {
  dispatchTerminalCellChunk,
  dispatchTerminalCellFrame,
  dispatchTerminalViewState,
} from "./terminal-stream.ts";
import {
  _handleSessionsEvent, _handlePresenceEvent, _handleWorkspacesDelta,
  _handleTasksDelta, _handlePermissionsDelta, _handleMcpEvent,
  _webhookDeltaSubs, _auditDeltaSubs,
} from "./sync-handlers.ts";
import {
  _workspaceProtoToWire, _taskProtoToWire, _webhookProtoToWire,
  _permProtoToWire, _mcpProtoToWire, _presenceProtoToWire,
} from "./sync-proto-adapters.ts";
import { setRoutableFps } from "./sync-routable.ts";

// T1.4 — last-seen event id (persisted to IndexedDB on a debounce). On
// reconnect, we send sinceEventId so coord backfills the gap. 0 = first
// connect ever or persisted state lost.
let _lastSeenEventId = 0;
const LAST_SEEN_DB_KEY = "roost.syncLastEventId";
try {
  const raw = typeof localStorage !== "undefined" ? localStorage.getItem(LAST_SEEN_DB_KEY) : null;
  if (raw) _lastSeenEventId = parseInt(raw, 10) || 0;
} catch { /* private mode */ }
let _persistTimer: ReturnType<typeof setTimeout> | null = null;
function _scheduleLastSeenPersist(): void {
  if (_persistTimer) return;
  _persistTimer = setTimeout(() => {
    _persistTimer = null;
    try { localStorage.setItem(LAST_SEEN_DB_KEY, String(_lastSeenEventId)); }
    catch { /* private mode */ }
  }, 1000);
}

/** Highest event id this tab has folded. _runConnectSync sends it as `since=`
 * so coord backfills whatever this tab missed while disconnected. */
export function lastSeenSyncEventId(): number {
  return _lastSeenEventId;
}

// Per-frame firehose dispatch — SHARED verbatim by the WebSocket transport
// in store/sync.ts. Every case preserved exactly, including the _lastSeenEventId bump +
// _scheduleLastSeenPersist() in the sessions/sessionEvent cases.
export function _dispatchSyncFrame(frame: FirehoseFrame): boolean {
  const k = frame.frame?.case as (typeof frame.frame.case) | "keepalive";
  if (!k) return false;
  const v = frame.frame.value;
  let consumed = true;
  // ONE reactive flush per frame: multi-write handlers (snapshot
  // fold, viewers list, session deletion) otherwise trigger a
  // downstream recompute per setRootStore call.
  batch(() => {
        switch (k) {
          case "sessions": {
            try {
              const src = v as { payloadJson: string }; // proto oneof value, case-narrowed
              const ev = JSON.parse(src.payloadJson) as { _event_id?: number };
              if (typeof ev._event_id === "number" && ev._event_id > _lastSeenEventId) {
                _lastSeenEventId = ev._event_id;
                _scheduleLastSeenPersist();
              }
              _handleSessionsEvent(ev);
            } catch (e) {
              signal("diag.corruption_signal", { kind: "sync_json_parse", frame: "sessions", msg: String(e), cooldownKey: "sync" });
              throw e;
            }
            break;
          }
          case "sessionEvent": {
            // T1.2 — proto-typed SessionEvent variant. Decode to the
            // legacy wire shape and feed the existing projector.
            const decoded = protoToEvent(v as never);
            if (!decoded) { consumed = false; break; }
            if (decoded._event_id > _lastSeenEventId) {
              _lastSeenEventId = decoded._event_id;
              _scheduleLastSeenPersist();
            }
            _handleSessionsEvent(decoded);
            break;
          }
          case "workerPresence": {
            const wire = _presenceProtoToWire(v as any);
            if (wire) _handlePresenceEvent(wire);
            else consumed = false;
            break;
          }
          case "workspaceDelta": {
            const wire = _workspaceProtoToWire(v as any);
            if (wire) _handleWorkspacesDelta(wire);
            else consumed = false;
            break;
          }
          case "taskDelta": {
            const wire = _taskProtoToWire(v as any);
            if (wire) _handleTasksDelta(wire);
            else consumed = false;
            break;
          }
          case "permissionDelta": {
            const wire = _permProtoToWire(v as any);
            if (wire) _handlePermissionsDelta(wire);
            else consumed = false;
            break;
          }
          case "mcpMsg": {
            const wire = _mcpProtoToWire(v as any);
            if (wire) _handleMcpEvent(wire);
            else consumed = false;
            break;
          }
          case "webhookTokenDelta": {
            const wire = _webhookProtoToWire(v as any);
            if (wire) {
              for (const sub of _webhookDeltaSubs) {
                try { sub(wire); } catch (e) { diag("sync.delta_sub_failed", { frame: "webhookToken", error: String(e) }); }
              }
            } else consumed = false;
            break;
          }
          case "auditRow": {
            // typed proto AuditRow → legacy wire shape for AuditLogPane.
            const r = v as { id: bigint; ts: bigint; callerFp?: string;
              callerLabel?: string; method: string; path: string;
              status: number; traceId?: string };
            const wire = {
              id: Number(r.id), ts: Number(r.ts),
              caller_fp: r.callerFp ?? null,
              caller_label: r.callerLabel ?? null,
              method: r.method, path: r.path, status: r.status,
              trace_id: r.traceId ?? null,
            };
            for (const sub of _auditDeltaSubs) {
              try { sub(wire); } catch (e) { diag("sync.delta_sub_failed", { frame: "audit", error: String(e) }); }
            }
            break;
          }
          case "coordinatorRelocation": {
            const relocation = v as { handoffId: string; targetUrl: string };
            void relocateBrowserToCoordinator(relocation.handoffId, relocation.targetUrl);
            break;
          }
          case "cellGrid": {
            const cell = v as PbCellGridFrame;
            diag("cell.recv", {
              sid: cell.sessionId,
              stream_id: cell.streamId,
              seq: Number(cell.seq),
            });
            dispatchTerminalCellFrame(cell);
            break;
          }
          case "cellGridChunk": {
            dispatchTerminalCellChunk(v as PbCellGridChunk);
            break;
          }
          case "terminalViewState": {
            dispatchTerminalViewState(v as TerminalViewStateFrame);
            break;
          }
          case "sessionPresence": {
            try {
              const p = v as { sessionId: string; payloadJson: string };
              const payload = JSON.parse(p.payloadJson) as { kind?: string; fps?: string[] };
              // kind="viewers" = the per-session presence list driven by
              // sessionsResize claims (coord/connect/router.ts viewer
              // tracker). Folded into rootStore.session_viewers; sidebar
              // SessionRow renders one dot per fp.
              if (payload && payload.kind === "viewers") {
                type Entry = { fp: string; cols: number; rows: number; lastMs?: number; label?: string; viewerKey?: string };
                const raw = payload as { entries?: Entry[]; fps?: string[] };
                const entries: Entry[] = Array.isArray(raw.entries)
                  ? raw.entries.map((e) => ({ ...e, viewerKey: e.viewerKey ?? e.fp }))
                  : Array.isArray(raw.fps)
                    ? raw.fps.map((fp) => ({ fp, viewerKey: fp, cols: 0, rows: 0 }))
                    : [];
                // SCD projection reads only session_viewers (min over entries);
                // the old latest_key tiebreak is gone, so a single write.
                // reconcile keyed by fp: stable entry identity → ViewersChip's
                // <For> keeps avatar DOM when the list content is unchanged.
                setRootStore("session_viewers", p.sessionId, reconcile(entries, { key: "fp" }));
              } else {
                _dispatchPresence(p.sessionId, payload);
              }
            } catch (e) {
              signal("diag.corruption_signal", { kind: "sync_json_parse", frame: "sessionPresence", msg: String(e), cooldownKey: "sync" });
              throw e;
            }
            break;
          }
          case "terminalTitle": {
            // Coord-authoritative OSC terminal title (terminal-title-hub).
            // Single source of truth — replaces the dead per-browser onTitle
            // path. sessionTitle()/SessionRow read rootStore.terminal_title[sid].
            const t = v as { sessionId: string; title: string };
            setRootStore("terminal_title", t.sessionId, t.title);
            break;
          }
          case "lastActivity": {
            // Coord-stamped last-activity ms (last-activity-hub). The sidebar
            // "Last activity" filter reads rootStore.last_activity[sid] to age
            // out idle OPEN sessions.
            const a = v as { sessionId: string; tsMs: number };
            setRootStore("last_activity", a.sessionId, a.tsMs);
            break;
          }
          case "agentStatus": {
            applyAgentStatusFrame(v as AgentStatusFrame);
            break;
          }
          case "workerRoutable": {
            // Live worker routability = coord's WS membership. Replaces the
            // stale workersList snapshot so an active server stops showing
            // red. workerOnline() reads this set; full-set replace semantics.
            const wr = v as { fps: string[] };
            setRoutableFps(new Set(wr.fps));
            break;
          }
          case "pairRequestDelta": {
            // Pair-request delta (perf sweep C2.4 — replaces the 5 s pairList
            // poller). `pending` upserts, `removedId` drops (approve/deny),
            // `snapshot` (seeded per Sync connect) REPLACES the whole set so
            // removals missed while disconnected can't linger.
            // Cast to the generated frame type: protobuf-es already decoded
            // the payload; the oneof case above pins the variant.
            const d = v as PairRequestDeltaProto;
            const fold = (p: { ephemeralId: string; label: string; createdAtMs: bigint }) =>
              setRootStore("pair_requests", p.ephemeralId, {
                ephemeral_id: p.ephemeralId, label: p.label, created_at_ms: Number(p.createdAtMs),
              });
            if (d.kind.case === "pending") fold(d.kind.value);
            else if (d.kind.case === "removedId") {
              setRootStore("pair_requests", d.kind.value, undefined as unknown as PairRequest);
            } else if (d.kind.case === "snapshot") {
              const keep = new Set<string>();
              for (const p of d.kind.value.pending) { keep.add(p.ephemeralId); fold(p); }
              // Kysely-style per-key delete: Solid setStore drops a key via an
              // undefined write (same idiom as the retired poller).
              for (const id of Object.keys(rootStore.pair_requests)) {
                if (!keep.has(id)) setRootStore("pair_requests", id, undefined as unknown as PairRequest);
              }
            }
            else consumed = false;
            break;
          }
          case "uiState": {
            // Browser tabs deliberately do not project peer UI state. Routing
            // and discarding this agent-facing frame is its full consumption.
            break;
          }
          case "uiCommand": {
            // ui-cc — agent-driven UI command (coord UiDispatch → ui_command
            // frame). Forwarded to the handler UiBridge registered with router
            // navigate bound; no bridge mounted → the command is deliberately
            // consumed as a no-op (the agent reads UiDispatch's `delivered`
            // count instead).
            _dispatchUiCommand(v as UiCommandFrame);
            break;
          }
          case "keepalive": {
            // Liveness heartbeat from coord. The watchdog's addEventListener
            // ("message") listener already reset lastMsgAt — no state change.
            break;
          }
          default: {
            consumed = false;
            break;
          }
        }
        });
  return consumed;
}
