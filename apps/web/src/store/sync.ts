// Bootstrap and live-sync: populates root store from coord tRPC.
// Phase 1: list queries → populate. Phase 2: single WS to /api/events → fold deltas.
// Phase 3: coord-health poller → window.__roostCoordHealth (read by ConnectionBanner).
// Called once from App.tsx on mount. R4.3 sync deliverable.

import { batch } from "solid-js";
import { reconcile } from "solid-js/store";
import { setRootStore, rootStore } from "./root.ts";
import type { PairRequest } from "./root.ts";
import type { PairRequestDeltaProto } from "@roost/shared/proto/events_pb";
import type { UiCommandFrame, FirehoseFrame, AgentStatusFrame } from "@roost/shared/proto/sync_pb";
import { FirehoseFrameSchema } from "@roost/shared/proto/sync_pb";
import { fromBinary } from "@bufbuild/protobuf";
import { protoToEvent } from "@roost/shared/wire/event-proto";
import { signCoordinatorJwt } from "../auth/web-key.ts";
import { getTabId } from "../auth/tab-id.ts";
import { _dispatchUiCommand } from "../lib/uiCommandDispatch.ts";
import { relocateBrowserToCoordinator } from "../auth/coordinator-relocation.ts";
import {
  _workspaceProtoToWire, _taskProtoToWire, _webhookProtoToWire,
  _permProtoToWire, _mcpProtoToWire, _presenceProtoToWire,
} from "./sync-proto-adapters.ts";
// tRPC client retired — queries/RPCs route through coordClient (Connect).
// The event firehose is _runConnectSync: a raw WebSocket to coord's
// /ws/coord-sync multiplexing 8 domain buses + PTY bytes as FirehoseFrames.
import type { Worker } from "@roost/shared/wire";
import { signal, diag } from "@roost/shared/diag";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import { _dispatchBytes, _dispatchCell, _dispatchPresence } from "./sync-dispatch.ts";
import { startStaleWatchdog, type StaleWatchdog } from "./sync-watchdog.ts";
import { applyAgentStatusFrame } from "./agent-status.ts";
// Worker-routability signal lives in sync-routable.ts (leaf): _runConnectSync
// writes it; the UI reads workerOnline (re-exported here so consumers keep
// importing it from store/sync.ts).
import { setOpen } from "./sync-stream-open.ts";
import { setRoutableFps } from "./sync-routable.ts";
export { workerOnline } from "./sync-routable.ts";
// Per-session cell/presence fan-out lives in sync-dispatch.ts (leaf module).
export {
  registerCellHandler, registerPresenceHandler, cellFrameCount, cellFullFrameCount,
  lastFullFrameSbRows, cellGridEpoch,
} from "./sync-dispatch.ts";
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

type SyncAbortReason = "visibility" | "manual" | "stale" | null;
interface LiveSyncLink {
  ws: WebSocket;
  gen: number;
  abortReason: SyncAbortReason;
  resolveClosed: () => void;
  closeEscapeTimer: ReturnType<typeof setTimeout> | null;
  watchdog: StaleWatchdog | null;
}

// Only the current dial is globally reachable. Every lifecycle resource stays
// on its owning record so late generation-N callbacks cannot mutate N+1.
let _liveLink: LiveSyncLink | null = null;
let _wsGen = 0;
let _smokeTransportPaused = false;
let _resumeSmokeTransport: (() => void) | null = null;

/** Milliseconds since the current OPEN Sync socket received any frame. */
export function syncLinkIdleMs(): number {
  const link = _liveLink;
  return link && link.ws.readyState === WebSocket.OPEN && link.watchdog
    ? link.watchdog.idleMs()
    : Number.POSITIVE_INFINITY;
}

/** How many Sync sockets this tab has dialed. */
export function syncWsGeneration(): number { return _wsGen; }

const WS_CLOSE_ESCAPE_MS = 5_000;

function _cleanupLink(link: LiveSyncLink): void {
  clearTimeout(link.closeEscapeTimer ?? undefined);
  link.closeEscapeTimer = null;
  link.watchdog?.stop();
  link.watchdog = null;
}

function _armCloseEscape(link: LiveSyncLink): void {
  clearTimeout(link.closeEscapeTimer ?? undefined);
  link.closeEscapeTimer = setTimeout(() => {
    link.closeEscapeTimer = null;
    if (_liveLink === link) setOpen(false);
    link.resolveClosed();
  }, WS_CLOSE_ESCAPE_MS);
}

function _initiateWsClose(reason: Exclude<SyncAbortReason, null>): void {
  const link = _liveLink;
  if (!link) return;
  link.abortReason = reason;
  if (_liveLink === link) setOpen(false);
  try { link.ws.close(); } catch { link.resolveClosed(); }
  _armCloseEscape(link);
}

export function _abortSyncForVisibility(): void {
  _initiateWsClose("visibility");
}


// Bounded reconnect protects the tab from a failed transport hot-loop. After
// SYNC_MAX_FAILURES the single owning loop parks in place until resumeSyncNow()
// wakes it, preserving the mounted terminal deck and its last painted grid.
const SYNC_MAX_FAILURES = 8;          // ~60 s with the exponential backoff below
let _syncFailures = 0;
let _syncPaused = false;
let _wakePausedSync: (() => void) | null = null;
/** Read-only view of the sync-paused flag for sync-bootstrap's refocus handler
 *  (a `let` can't be reassigned across a module boundary, but a getter reads
 *  the live value). */
export function isSyncPaused(): boolean { return _syncPaused; }
// Signal published on window for ConnectionBanner / other consumers.
function _writeSyncPaused(paused: boolean): void {
  (window as Window & { __roostSyncPaused?: boolean }).__roostSyncPaused = paused;
}

/** Manually recover the existing Sync loop and replace any live stale socket
 *  without remounting the SPA or its persistent terminal deck. */
export function reconnectNow(): void {
  console.info("[sync.connect] manual reconnect requested");
  resumeSyncNow();
  _initiateWsClose("manual");
}


// ─── pre-hydration queue ──────────────────────────────────────────────────
// The Sync socket is dialed in PARALLEL with the bootstrap's list RPCs (it
// needs nothing but a JWT and the persisted event cursor), which shaves a full
// round trip off first paint. That creates one ordering hazard: a session delta
// arriving before sessionsList's snapshot is applied would be clobbered by that
// older snapshot. So every frame decoded before hydration is held here and
// replayed, in arrival order, through the same dispatch switch.
const PRE_HYDRATION_CAP = 5000;
let _queueingPreHydration = true;
let _preHydrationQueue: FirehoseFrame[] = [];
let _preHydrationOverflow = false;

/** Replay everything held since the socket opened, then let frames flow live.
 *  Called by sync-bootstrap the instant the sessions snapshot lands — and also
 *  from its failure path, so frames are never stranded by a bootstrap that
 *  never hydrates (a cell frame for an unknown session drops silently by
 *  design, so draining into an empty store is harmless). */
export function drainPreHydration(): void {
  _queueingPreHydration = false;
  const queued = _preHydrationQueue;
  _preHydrationQueue = [];
  for (const frame of queued) _dispatchSyncFrame(frame);
  if (!_preHydrationOverflow) return;
  // The queue overflowed, so an unknown set of deltas was dropped. A re-dial
  // replays them from the persisted cursor via `since=`, which is the only way
  // back to a truthful store.
  _preHydrationOverflow = false;
  signal("sync.prehydration_overflow", { cap: PRE_HYDRATION_CAP, cooldownKey: "sync" });
  forceSyncReconnect();
}

// ─── first-open barrier + backoff wake ────────────────────────────────────
// A fresh browser sends `since=0`, and coord deliberately runs NO backfill for a
// zero cursor (the bootstrap's list snapshots already carry that state). So any
// event published between the sessions snapshot and this socket opening is lost
// outright — there is nothing to replay it from. The bootstrap therefore waits
// for the first open before applying the snapshot, which closes the window
// instead of merely shrinking it.
let _resolveFirstOpen: (() => void) | null = null;
const _firstOpen = new Promise<void>((resolve) => { _resolveFirstOpen = resolve; });

/** Resolves on the Sync socket's first successful open, or after `timeoutMs` —
 *  bounded so a coord that answers unary RPCs but refuses the WS upgrade cannot
 *  leave the store unhydrated (a blank app). */
export function syncSocketOpened(timeoutMs: number): Promise<void> {
  return Promise.race([_firstOpen, new Promise<void>((resolve) => setTimeout(resolve, timeoutMs))]);
}

// Immediate re-dial request. Read by whichever side sees it first, so it works
// whether the reconnect loop is already parked in its backoff sleep or has not
// reached it yet.
let _resumeRequested = false;
let _wakeBackoff: (() => void) | null = null;

/** Re-dial NOW after authorization, refocus, or an explicit reconnect. Wakes
 * both exhaustion parking and an ordinary reconnect backoff. */
export function resumeSyncNow(): void {
  _syncPaused = false;
  _writeSyncPaused(false);
  _syncFailures = 0;
  _resumeRequested = true;
  const wakePaused = _wakePausedSync;
  _wakePausedSync = null;
  wakePaused?.();
  const wakeBackoff = _wakeBackoff;
  _wakeBackoff = null;
  wakeBackoff?.();
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
    if (_smokeTransportPaused) {
      const { promise: resumed, resolve } = Promise.withResolvers<void>();
      _resumeSmokeTransport = resolve;
      await resumed;
      if (_resumeSmokeTransport === resolve) _resumeSmokeTransport = null;
    }
    if (_syncPaused) {
      console.warn("[sync.connect] paused — waiting for in-place resume");
      const { promise: resumed, resolve } = Promise.withResolvers<void>();
      _wakePausedSync = resolve;
      await resumed;
      if (_wakePausedSync === resolve) _wakePausedSync = null;
    }
    if (_resumeRequested) {
      _resumeRequested = false;
      backoff = 1000;
    }
    let dialLink: LiveSyncLink | null = null;
    let abortReason: SyncAbortReason = null;
    try {
      console.debug("[sync.connect] starting Sync stream", { sinceEventId: _lastSeenEventId, attempt: _syncFailures + 1 });
      // Coord base: localStorage override (multi-coord testing) else same-origin.
      // http→ws / https→wss. The device JWT travels as a WebSocket
      // subprotocol so proxies never log it in the URL.
      const override = typeof localStorage !== "undefined" ? localStorage.getItem("roost.coordinatorUrl") : null;
      const wsBase = (override || location.origin).replace(/^http/, "ws");
      const jwt = await signCoordinatorJwt();
      // tab= gives coord a per-TAB identity for this socket, the same
      // `${fingerprint}:${tabId}` key sessionsResize claims use, so the cell +
      // byte fanout can ship only the sessions THIS tab actually claimed.
      const url = `${wsBase}/ws/coord-sync?since=${_lastSeenEventId}&tab=${encodeURIComponent(getTabId())}`;
      const ws = new WebSocket(url, ["roost-auth", jwt]);
      ws.binaryType = "arraybuffer";
      const gen = ++_wsGen;
      const { promise: closed, resolve: resolveClosed } = Promise.withResolvers<void>();
      const link: LiveSyncLink = {
        ws,
        gen,
        abortReason: null,
        resolveClosed,
        closeEscapeTimer: null,
        watchdog: null,
      };
      dialLink = link;
      _liveLink = link;
      ws.onopen = () => {
        if (_liveLink !== link) {
          try { ws.close(); } catch { /* obsolete dial */ }
          return;
        }
        // A successful open = upgrade + JWT verify succeeded (the WS analog of
        // the old "reset on first frame"). Clear the failure counter + backoff.
        _syncFailures = 0;
        _noteSyncConnect();
        backoff = 1000;
        setOpen(true);
        _resolveFirstOpen?.();
        _resolveFirstOpen = null;
        link.watchdog = startStaleWatchdog(ws, {
          onStale: () => {
            if (_liveLink === link) _initiateWsClose("stale");
          },
        });
      };
      ws.onmessage = (ev) => {
        if (_liveLink !== link) return;
        try {
          const frame = fromBinary(FirehoseFrameSchema, new Uint8Array(ev.data as ArrayBuffer));
          if (_queueingPreHydration) {
            if (_preHydrationQueue.length >= PRE_HYDRATION_CAP) {
              _preHydrationQueue = [];
              _preHydrationOverflow = true;
            } else {
              _preHydrationQueue.push(frame);
            }
            return;
          }
          _dispatchSyncFrame(frame);
        } catch (e) {
          signal("diag.corruption_signal", { kind: "sync_ws_decode", frame: "firehose", msg: String(e), cooldownKey: "sync" });
        }
      };
      ws.onerror = (e) => { console.debug("[sync.connect] ws error", e); };
      ws.onclose = () => {
        _cleanupLink(link);
        if (_liveLink === link) setOpen(false);
        link.resolveClosed();
      };
      await closed;
      console.debug("[sync.connect] stream ended; re-dialing");
    } catch (err) {
      console.debug("[sync.connect] stream error; backing off", err);
    } finally {
      if (dialLink) {
        abortReason = dialLink.abortReason;
        _cleanupLink(dialLink);
        if (_liveLink === dialLink) {
          _liveLink = null;
          setOpen(false);
        }
      }
    }
    // Visibility / manual / watchdog closes are intentional and skip failures.
    if (abortReason === "visibility" || abortReason === "manual" || abortReason === "stale") {
      backoff = 1000;
      continue;
    }
    _syncFailures += 1;
    if (_syncFailures >= SYNC_MAX_FAILURES) {
      console.warn(`[sync.connect] ${SYNC_MAX_FAILURES} consecutive failures — parking Sync loop.`);
      signal("reconnect.give_up", { failures: _syncFailures, action: "pause" });
      _syncPaused = true;
      _writeSyncPaused(true);
      continue;
    }
    // resumeSyncNow may land before the sleep starts or while it is parked;
    // both are checked so neither ordering loses the wake.
    if (_resumeRequested) { _resumeRequested = false; backoff = 1000; continue; }
    const { promise: slept, resolve: wake } = Promise.withResolvers<void>();
    _wakeBackoff = wake;
    const sleepTimer = setTimeout(wake, backoff);
    await slept;
    clearTimeout(sleepTimer);
    _wakeBackoff = null;
    if (_resumeRequested) { _resumeRequested = false; backoff = 1000; continue; }
    backoff = Math.min(backoff * 2, 30_000);
  }
}
/** Test-only: force-close the live firehose WS so the reconnect loop re-dials. */
export function forceSyncReconnect(): void { _initiateWsClose("manual"); }

/** Smoke-only: put the owning Sync loop into its real retry-exhausted parked
 * state, then close the current tube so the loop reaches that park. Recovery
 * remains exclusively resumeSyncNow()/the refocus path under test. */
export function forceSyncRetryExhausted(): void {
  if (typeof localStorage === "undefined" || localStorage.getItem("roostSmoke") !== "1") return;
  _syncFailures = SYNC_MAX_FAILURES;
  _syncPaused = true;
  _writeSyncPaused(true);
  _initiateWsClose("manual");
}

/** Smoke-only partition gate. Close the current tube and hold re-dial until the
 * paired resume, allowing the real PTY to diverge from the browser consumer. */
export function pauseSyncTransport(): void {
  if (typeof localStorage === "undefined" || localStorage.getItem("roostSmoke") !== "1") return;
  _smokeTransportPaused = true;
  _initiateWsClose("manual");
}

export function resumeSyncTransport(): void {
  if (typeof localStorage === "undefined" || localStorage.getItem("roostSmoke") !== "1") return;
  _smokeTransportPaused = false;
  _resumeSmokeTransport?.();
  _resumeSmokeTransport = null;
  resumeSyncNow();
}

