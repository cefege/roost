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
import type { FirehoseFrame } from "@roost/shared/proto/sync_pb";
import { protoToEvent } from "@roost/shared/wire/event-proto";
import { relocateBrowserToCoordinator } from "../auth/coordinator-relocation.ts";
import { _dispatchUiCommand } from "../lib/uiCommandDispatch.ts";
import { applyAgentStatusFrame } from "./agent-status.ts";
import { deleteStoreRecord, rootStore, setRootStore } from "./root.ts";
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

/** Fold one typed-bus delta through its proto→wire adapter, converting any
 *  throw into a dropped frame plus a corruption signal. _consumeSyncFrame
 *  treats a dispatch throw as fatal protocol damage and tears down the v2
 *  socket — one malformed *_json column or shape drift must cost ONE frame,
 *  never the stream. */
function _foldDelta(
  frame: string,
  adapt: () => unknown,
  deliver: (wire: unknown) => void,
): boolean {
  try {
    const wire = adapt();
    if (!wire) return false;
    deliver(wire);
    return true;
  } catch (error) {
    signal("diag.corruption_signal", {
      kind: "sync_delta_dropped",
      frame,
      msg: String(error),
      cooldownKey: "sync",
    });
    return false;
  }
}

export function _dispatchSyncFrame(frame: FirehoseFrame): boolean {
  const oneof = frame.frame;
  if (!oneof) return false;
  let consumed = true;
  // ONE reactive flush per frame: multi-write handlers (snapshot
  // fold, viewers list, session deletion) otherwise trigger a
  // downstream recompute per setRootStore call.
  batch(() => {
    switch (oneof.case) {
      case "sessions": {
        // Deliberately fatal: a sessions payload coord itself persisted must
        // parse; folding garbage would poison the store far worse than one
        // reconnect. _consumeSyncFrame catches and closes the link.
        try {
          const ev = JSON.parse(oneof.value.payloadJson) as { _event_id?: number };
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
        const decoded = protoToEvent(oneof.value);
        if (!decoded) { consumed = false; break; }
        if (decoded._event_id > _lastSeenEventId) {
          _lastSeenEventId = decoded._event_id;
          _scheduleLastSeenPersist();
        }
        _handleSessionsEvent(decoded);
        break;
      }
      case "workerPresence":
        consumed = _foldDelta("workerPresence",
          () => _presenceProtoToWire(oneof.value),
          (wire) => _handlePresenceEvent(wire));
        break;
      case "workspaceDelta":
        consumed = _foldDelta("workspaceDelta",
          () => _workspaceProtoToWire(oneof.value),
          (wire) => _handleWorkspacesDelta(wire));
        break;
      case "taskDelta":
        consumed = _foldDelta("taskDelta",
          () => _taskProtoToWire(oneof.value),
          (wire) => _handleTasksDelta(wire));
        break;
      case "permissionDelta":
        consumed = _foldDelta("permissionDelta",
          () => _permProtoToWire(oneof.value),
          (wire) => _handlePermissionsDelta(wire));
        break;
      case "mcpMsg":
        consumed = _foldDelta("mcpMsg",
          () => _mcpProtoToWire(oneof.value),
          (wire) => _handleMcpEvent(wire));
        break;
      case "webhookTokenDelta":
        consumed = _foldDelta("webhookTokenDelta", () => _webhookProtoToWire(oneof.value), (wire) => {
          for (const sub of _webhookDeltaSubs) {
            try { sub(wire); } catch (e) { diag("sync.delta_sub_failed", { frame: "webhookToken", error: String(e) }); }
          }
        });
        break;
      case "auditRow": {
        // typed proto AuditRow → legacy wire shape for AuditLogPane.
        const r = oneof.value;
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
        const relocation = oneof.value;
        void relocateBrowserToCoordinator(relocation.handoffId, relocation.targetUrl);
        break;
      }
      case "cellGrid": {
        const cell = oneof.value;
        diag("cell.recv", {
          sid: cell.sessionId,
          stream_id: cell.streamId,
          seq: Number(cell.seq),
        });
        dispatchTerminalCellFrame(cell);
        break;
      }
      case "cellGridChunk":
        dispatchTerminalCellChunk(oneof.value);
        break;
      case "terminalViewState":
        dispatchTerminalViewState(oneof.value);
        break;
      case "sessionPresence": {
        try {
          const p = oneof.value;
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
        const t = oneof.value;
        setRootStore("terminal_title", t.sessionId, t.title);
        break;
      }
      case "lastActivity": {
        // Coord-stamped last-activity ms (last-activity-hub). The sidebar
        // "Last activity" filter reads rootStore.last_activity[sid] to age
        // out idle OPEN sessions.
        const a = oneof.value;
        setRootStore("last_activity", a.sessionId, a.tsMs);
        break;
      }
      case "agentStatus":
        applyAgentStatusFrame(oneof.value);
        break;
      case "workerRoutable": {
        // Live worker routability = coord's WS membership. Replaces the
        // stale workersList snapshot so an active server stops showing
        // red. workerOnline() reads this set; full-set replace semantics.
        setRoutableFps(new Set(oneof.value.fps));
        break;
      }
      case "pairRequestDelta": {
        // Pair-request delta (perf sweep C2.4 — replaces the 5 s pairList
        // poller). `pending` upserts, `removedId` drops (approve/deny),
        // `snapshot` (seeded per Sync connect) REPLACES the whole set so
        // removals missed while disconnected can't linger.
        const d = oneof.value;
        const fold = (p: { ephemeralId: string; label: string; createdAtMs: bigint }) =>
          setRootStore("pair_requests", p.ephemeralId, {
            ephemeral_id: p.ephemeralId, label: p.label, created_at_ms: Number(p.createdAtMs),
          });
        if (d.kind.case === "pending") fold(d.kind.value);
        else if (d.kind.case === "removedId") {
          deleteStoreRecord("pair_requests", d.kind.value);
        } else if (d.kind.case === "snapshot") {
          const keep = new Set<string>();
          for (const p of d.kind.value.pending) { keep.add(p.ephemeralId); fold(p); }
          for (const id of Object.keys(rootStore.pair_requests)) {
            if (!keep.has(id)) deleteStoreRecord("pair_requests", id);
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
        _dispatchUiCommand(oneof.value);
        break;
      }
      case "keepalive": {
        // Liveness heartbeat from coord. The watchdog's message listener
        // already reset lastMsgAt — no state change.
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
