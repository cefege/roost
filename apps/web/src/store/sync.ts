// Bootstrap and live-sync: populates root store from coord tRPC.
// Phase 1: list queries → populate. Phase 2: single WS to /api/events → fold deltas.
// Phase 3: coord-health poller → window.__roostCoordHealth (read by ConnectionBanner).
// Called once from App.tsx on mount. R4.3 sync deliverable.

import { batch } from "solid-js";
import { reconcile } from "solid-js/store";
import { setRootStore, rootStore } from "./root.ts";
import type { PairRequest } from "./root.ts";
import type { PairRequestDeltaProto } from "@roost/shared/proto/events_pb";
import type { UiCommandFrame, FirehoseFrame, AgentEntriesFrame } from "@roost/shared/proto/sync_pb";
import { FirehoseFrameSchema } from "@roost/shared/proto/sync_pb";
import { fromBinary } from "@bufbuild/protobuf";
import { protoToEvent } from "@roost/shared/wire/event-proto";
import { agentEntryFromProto } from "@roost/shared/wire/agent-proto";
import { upsertEntries } from "./agentEntries.ts";
import { signCoordinatorJwt } from "../auth/web-key.ts";
import { _dispatchUiCommand } from "../lib/uiCommandDispatch.ts";
import { relocateBrowserToCoordinator } from "../auth/coordinator-relocation.ts";
import {
  _workspaceProtoToWire, _taskProtoToWire, _webhookProtoToWire,
  _permProtoToWire, _mcpProtoToWire, _presenceProtoToWire,
} from "./sync-proto-adapters.ts";
import { isPageVisible } from "../lib/pageVisible.ts";
// tRPC client retired — queries/RPCs route through coordClient (Connect).
// The event firehose is _runConnectSync: a raw WebSocket to coord's
// /ws/coord-sync multiplexing 8 domain buses + PTY bytes as FirehoseFrames.
import type { Worker } from "@roost/shared/wire";
import { signal, diag } from "@roost/shared/diag";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import { _dispatchBytes, _dispatchCell, _dispatchPresence } from "./sync-dispatch.ts";
import { startStaleWatchdog } from "./sync-watchdog.ts";
// Worker-routability signal lives in sync-routable.ts (leaf): _runConnectSync
// writes it; the UI reads workerOnline (re-exported here so consumers keep
// importing it from store/sync.ts).
import { setOpen } from "./sync-stream-open.ts";
import { setRoutableFps } from "./sync-routable.ts";
export { workerOnline } from "./sync-routable.ts";
// Per-session cell/presence fan-out lives in sync-dispatch.ts (leaf module).
export { registerCellHandler, registerPresenceHandler, cellFrameCount } from "./sync-dispatch.ts";
// Per-domain delta handlers + keeper-death detector + delta-sub registries
// live in sync-handlers.ts; the firehose calls them and iterates the sub Sets.
import {
  _noteSyncConnect,
  _handleSessionsEvent, _handlePresenceEvent, _handleWorkspacesDelta,
  _handleTasksDelta, _handlePermissionsDelta, _handleMcpEvent,
  _webhookDeltaSubs, _auditDeltaSubs,
} from "./sync-handlers.ts";
export { registerWebhookDelta, registerAuditDelta } from "./sync-handlers.ts";




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

// Live WebSocket for the in-flight Sync feed. Refreshed every reconnect.
// Exposed at module scope so the visibilitychange handler can drop the
// socket when the tab returns from background — Chrome's background-tab
// throttle can silently stall a long-lived socket without surfacing an
// error, leaving the SPA stuck on the last delivered frame. Closing forces
// ws.onclose → the reconnect loop re-dials with sinceEventId, which
// backfills every event (PTY bytes included) we missed. Closing (not
// aborting an HTTP stream) also routes teardown through Bun's
// websocket.close callback rather than RequestContext.onAbort — the
// server-side UAF this transport migration exists to dodge.
let _liveWs: WebSocket | null = null;
let _syncAbortReason: "visibility" | "manual" | "stale" | null = null;
// Generation counter so stale onclose/onopen from an abandoned socket can't
// clobber the current socket's state.
let _wsGen = 0;

// Close-escape: if the close handshake hangs (e.g., dead TCP after sleep),
// force-resolve after WS_CLOSE_ESCAPE_MS so the reconnect loop isn't wedged.
const WS_CLOSE_ESCAPE_MS = 5_000;
let _resolveClosed: (() => void) | null = null;
let _closeEscapeTimer: ReturnType<typeof setTimeout> | null = null;

function _armCloseEscape(): void {
  if (_closeEscapeTimer) clearTimeout(_closeEscapeTimer);
  _closeEscapeTimer = setTimeout(() => {
    _closeEscapeTimer = null;
    setOpen(false);            // rising edge even if onclose is delayed
    _resolveClosed?.();        // unstick `await closed` (hung close handshake)
  }, WS_CLOSE_ESCAPE_MS);
}

// Consolidate all close-initiation — no scattered _liveWs.close() calls.
function _initiateWsClose(reason: "visibility" | "manual" | "stale"): void {
  if (!_liveWs) return;
  _syncAbortReason = reason;
  setOpen(false);
  _liveWs.close();
  _armCloseEscape();
}
let _stopWatchdog: (() => void) | null = null;
export function _abortSyncForVisibility(): void {
  _initiateWsClose("visibility");
}


// Bounded-reconnect state (Author 2026-06-17: 'how do we propose this
// doesn't happen again' — the SPA's Sync loop was retrying forever
// after coord rotated its TLS cert, the broken request hot-looped,
// the JS thread saturated, and the tab wedged. Cap retries at
// SYNC_MAX_FAILURES → set _syncPaused → SPA shows a Reconnect button
// that calls reconnectNow() → location.reload()).
const SYNC_MAX_FAILURES = 8;          // ~60 s with the exponential backoff below
let _syncFailures = 0;
let _syncPaused = false;
/** Read-only view of the sync-paused flag for sync-bootstrap's refocus handler
 *  (a `let` can't be reassigned across a module boundary, but a getter reads
 *  the live value). */
export function isSyncPaused(): boolean { return _syncPaused; }
// Signal published on window for ConnectionBanner / other consumers.
function _writeSyncPaused(paused: boolean): void {
  (window as Window & { __roostSyncPaused?: boolean }).__roostSyncPaused = paused;
}

/** Manually trigger a reconnect after the bounded-retry loop has given
 *  up — reloads the page (fresh sync stream + health poller). Called by
 *  the Reconnect button in ConnectionBanner. */
// Reconnect button entrypoint — a full page reload re-establishes both
// the sync stream and the health poller (the banner is gated on the
// latter, not the sync stream). Author 2026-06-18: "reconnect button
// seems to be dead."
export function reconnectNow(): void {
  console.info("[sync.connect] manual reconnect requested — reloading page");
  location.reload();
}

// Per-frame firehose dispatch — SHARED verbatim by the WebSocket transport
// below. Every case preserved exactly, including the _lastSeenEventId bump +
// _scheduleLastSeenPersist() in the sessions/sessionEvent cases.
function _dispatchSyncFrame(frame: FirehoseFrame): void {
  const k = frame.frame?.case as (typeof frame.frame.case) | "keepalive";
  if (!k) return;
  const v = frame.frame.value;
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
            }
            break;
          }
          case "sessionEvent": {
            // T1.2 — proto-typed SessionEvent variant. Decode to the
            // legacy wire shape and feed the existing projector.
            const decoded = protoToEvent(v as never);
            if (!decoded) break;
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
            break;
          }
          case "workspaceDelta": {
            const wire = _workspaceProtoToWire(v as any);
            if (wire) _handleWorkspacesDelta(wire);
            break;
          }
          case "taskDelta": {
            const wire = _taskProtoToWire(v as any);
            if (wire) _handleTasksDelta(wire);
            break;
          }
          case "permissionDelta": {
            const wire = _permProtoToWire(v as any);
            if (wire) _handlePermissionsDelta(wire);
            break;
          }
          case "mcpMsg": {
            const wire = _mcpProtoToWire(v as any);
            if (wire) _handleMcpEvent(wire);
            break;
          }
          case "webhookTokenDelta": {
            const wire = _webhookProtoToWire(v as any);
            if (wire) {
              for (const sub of _webhookDeltaSubs) {
                try { sub(wire); } catch (e) { diag("sync.delta_sub_failed", { frame: "webhookToken", error: String(e) }); }
              }
            }
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
          case "bytes": {
            const b = v as { sessionId: string; data: Uint8Array; seq: bigint };
            _dispatchBytes(b.sessionId, b.data);
            break;
          }
          case "cellGrid": {
            // R11 — pre-rendered cell frame. CellGridRenderer (cell
            // mode) consumes it; no-op for byte-mode viewers (no handler).
            diag("cell.recv", { sid: (v as PbCellGridFrame).sessionId || "", seq: Number((v as PbCellGridFrame).seq || 0) });
            _dispatchCell(v as PbCellGridFrame);
            break;
          }
          case "agentEntries": {
            // Agent-session transcript deltas (omp RPC projection). Volatile,
            // presence-class: no durable replay, so this carries only the live
            // tail — history comes from SessionsGetAgentEntries. Entries upsert
            // by `seq`, which is what makes a replayed window idempotent.
            const af = v as AgentEntriesFrame;
            try {
              upsertEntries(af.sessionId, af.entries.map(agentEntryFromProto));
            } catch (e) {
              // agentEntryFromProto re-Zod-parses: a worker/SPA enum drift
              // surfaces loudly here instead of poisoning the transcript.
              signal("diag.corruption_signal", { kind: "agent_entry_decode", frame: "agentEntries", msg: String(e), cooldownKey: "sync" });
            }
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
            break;
          }
          case "uiCommand": {
            // ui-cc — agent-driven UI command (coord UiDispatch → ui_command
            // frame). Forwarded to the handler UiBridge registered with router
            // navigate bound; no bridge mounted → drops silently (the agent
            // reads UiDispatch's `delivered` count instead). `uiState` frames
            // are this SPA's own reflection — deliberately NO case for them
            // (agents consume them via UiListStates / their own Sync stream);
            // they fall through the switch silently like any unknown frame.
            _dispatchUiCommand(v as UiCommandFrame);
            break;
          }
          case ("keepalive" as const): {
            // Liveness heartbeat from coord. The watchdog's addEventListener
            // ("message") listener already reset lastMsgAt — no state change.
            // NOTE: Type assertion required because keepalive is not in the
            // generated FirehoseFrame proto but is sent by coord.
            break;
          }
        }
        });
}

// Firehose transport — a raw browser WebSocket to coord's /ws/coord-sync.
// Was a Connect server-streaming Sync RPC; moved to a WebSocket to dodge a
// Bun v1.3.14 use-after-free in RequestContext.onAbort that crashed the
// coordinator whenever the browser aborted the long-lived streaming response.
// A WS close routes teardown through Bun's websocket.close callback, never
// RequestContext.onAbort. Frames are byte-identical FirehoseFrame protos —
// only the tube changed; _dispatchSyncFrame above is shared verbatim.
export async function _runConnectSync(): Promise<void> {
  let backoff = 1000;
  while (true) {
    if (_syncPaused) {
      console.warn("[sync.connect] paused — waiting for manual reconnect (page reload)");
      return;
    }
    try {
      console.debug("[sync.connect] starting Sync stream", { sinceEventId: _lastSeenEventId, attempt: _syncFailures + 1 });
      // Coord base: localStorage override (multi-coord testing) else same-origin.
      // http→ws / https→wss. Auth via query-param JWT (the browser WebSocket
      // API can't set headers); since=<lastEventId> backfills the gap coord
      // missed while we were away.
      const override = typeof localStorage !== "undefined" ? localStorage.getItem("roost.coordinatorUrl") : null;
      const wsBase = (override || location.origin).replace(/^http/, "ws");
      const jwt = await signCoordinatorJwt();
      const url = `${wsBase}/ws/coord-sync?token=${encodeURIComponent(jwt)}&since=${_lastSeenEventId}`;
      const ws = new WebSocket(url);
      ws.binaryType = "arraybuffer";
      _liveWs = ws;
      const gen = ++_wsGen;
      const { promise: closed, resolve: onClosed } = Promise.withResolvers<void>();
      _resolveClosed = onClosed;
      ws.onopen = () => {
        // A successful open = upgrade + JWT verify succeeded (the WS analog of
        // the old "reset on first frame"). Clear the failure counter + backoff.
        _syncFailures = 0;
        _noteSyncConnect();
        backoff = 1000;
        if (gen === _wsGen) setOpen(true);
        _stopWatchdog = startStaleWatchdog(ws, { onStale: () => { _initiateWsClose("stale"); } });
      };
      ws.onmessage = (ev) => {
        try {
          const frame = fromBinary(FirehoseFrameSchema, new Uint8Array(ev.data as ArrayBuffer));
          _dispatchSyncFrame(frame);
        } catch (e) {
          signal("diag.corruption_signal", { kind: "sync_ws_decode", frame: "firehose", msg: String(e), cooldownKey: "sync" });
        }
      };
      ws.onerror = (e) => { console.debug("[sync.connect] ws error", e); };
      ws.onclose = () => {
        if (_closeEscapeTimer) { clearTimeout(_closeEscapeTimer); _closeEscapeTimer = null; }
        if (_stopWatchdog) { _stopWatchdog(); _stopWatchdog = null; }
        if (gen === _wsGen) setOpen(false);
        onClosed();
      };
      await closed;
      console.debug("[sync.connect] stream ended; re-dialing");
    } catch (err) {
      console.debug("[sync.connect] stream error; backing off", err);
    } finally {
      _liveWs = null;
      _resolveClosed = null;
      if (_stopWatchdog) { _stopWatchdog(); _stopWatchdog = null; }
    }
    // Visibility / manual aborts skip the failure counter — they're
    // user-initiated, not failures.
    if (_syncAbortReason === "visibility" || _syncAbortReason === "manual" || _syncAbortReason === "stale") {
      _syncAbortReason = null;
      backoff = 1000;
      continue;
    }
    _syncFailures += 1;
    if (_syncFailures >= SYNC_MAX_FAILURES) {
      console.warn(`[sync.connect] ${SYNC_MAX_FAILURES} consecutive failures — reloading page.`);
      signal("reconnect.give_up", { failures: _syncFailures, action: "reload" });
      // ponytail: reload the page instead of showing a "Reconnect" banner.
      // After a coord restart, Chrome's TLS session ticket cache and HTTP/2
      // connection pool hold stale state that prevents reconnection even with
      // fresh AbortController + new request. A page reload clears everything:
      // new TCP, new TLS handshake, new HTTP/2 session. The ~1s reload is
      // faster than the user noticing the banner + clicking Reconnect + waiting.
      // Only reload foreground tabs — background tabs reload on focus.
      if (isPageVisible()) {
        location.reload();
      } else {
        // Defer reload until the user switches back.
        _syncPaused = true;
        _writeSyncPaused(true);
      }
      return;
    }
    const { promise: slept, resolve: wake } = Promise.withResolvers<void>();
    setTimeout(wake, backoff);
    await slept;
    backoff = Math.min(backoff * 2, 30_000);
  }
}
/** Test-only: force-close the live firehose WS so the reconnect loop re-dials.
 *  Mirrors forceVisible for the smoke harness. */
export function forceSyncReconnect(): void { _initiateWsClose("manual"); }

