// Bootstrap and live-sync: populates root store from coord tRPC.
// Phase 1: list queries → populate. Phase 2: single WS to /api/events → fold deltas.
// Phase 3: coord-health poller → window.__roostCoordHealth (read by ConnectionBanner).
// Called once from App.tsx on mount. R4.3 sync deliverable.

import { batch } from "solid-js";
import { create, toBinary } from "@bufbuild/protobuf";
import { reconcile } from "solid-js/store";
import { setRootStore, rootStore } from "./root.ts";
import type { PairRequest } from "./root.ts";
import type { PairRequestDeltaProto } from "@roost/shared/proto/events_pb";
import {
  SyncClientFrameSchema,
  SyncDomainReadyCommandSchema,
  SyncDomainSubscriptionCommandSchema,
  SyncDomain,
  type SyncClientFrame,
  type UiCommandFrame,
  type SyncSubscribedFrame,
  type SyncDomainResetFrame,
  type WorkerRoutableFrame,
  type FirehoseFrame,
  type AgentStatusFrame,
} from "@roost/shared/proto/sync_pb";
import { protoToEvent } from "@roost/shared/wire/event-proto";
import { signCoordinatorJwt } from "../auth/web-key.ts";
import { getTabId } from "../auth/tab-id.ts";
import { _dispatchUiCommand } from "../lib/uiCommandDispatch.ts";
import { relocateBrowserToCoordinator } from "../auth/coordinator-relocation.ts";
import { markPhase } from "../lib/diag.ts";
import {
  _workspaceProtoToWire, _taskProtoToWire, _webhookProtoToWire,
  _permProtoToWire, _mcpProtoToWire, _presenceProtoToWire,
} from "./sync-proto-adapters.ts";
// tRPC client retired — queries/RPCs route through coordClient (Connect).
// The event firehose is _runConnectSync: a raw WebSocket to coord's
// /ws/coord-sync carrying state, cell grids, and compact terminal metadata.
import type { Worker } from "@roost/shared/wire";
import { signal, diag } from "@roost/shared/diag";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import {
  _dispatchCell, _dispatchPresence, _dispatchTerminalLink,
  resetCellMountBuffers,
} from "./sync-dispatch.ts";
import { startStaleWatchdog, type StaleWatchdog } from "./sync-watchdog.ts";
import {
  canAcceptSyncLink,
  canOpenSyncLink,
  decodeFirehoseFrame,
  dispatchSyncFrameCausally,
  isImmediateSyncRedial,
  isSyncBackpressureClose,
} from "./sync-flow.ts";
import { applyAgentStatusFrame } from "./agent-status.ts";
// Worker-routability signal lives in sync-routable.ts (leaf): _runConnectSync
// writes it; the UI reads workerOnline (re-exported here so consumers keep
// importing it from store/sync.ts).
import { setOpen } from "./sync-stream-open.ts";
import { setRoutableFps } from "./sync-routable.ts";
export { workerOnline } from "./sync-routable.ts";
// Per-session cell/presence fan-out lives in sync-dispatch.ts (leaf module).
export {
  registerCellHandler, registerPresenceHandler, setCellMountClaimActive,
  cellFrameCount, cellFullFrameCount, lastFullFrameSbRows, cellGridEpoch,
  cellMountBufferStats,
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

type SyncAbortReason = "visibility" | "manual" | "stale" | "flow" | null;

interface SyncV2DomainState {
  generation: bigint;
  subscribed: boolean;
  ready: boolean;
}

interface SyncV2LinkState {
  socketId: string;
  processEpoch: string;
  domains: Map<SyncDomain, SyncV2DomainState>;
  routableChunks: Map<string, {
    count: number;
    chunks: Array<string[] | undefined>;
  }>;
}

interface LiveSyncLink {
  ws: WebSocket;
  gen: number;
  abortReason: SyncAbortReason;
  accepting: boolean;
  resolveClosed: () => void;
  expectsV2: boolean;
  closeEscapeTimer: ReturnType<typeof setTimeout> | null;
  watchdog: StaleWatchdog | null;
  v2: SyncV2LinkState | null;
}

export interface SyncDomainToken {
  readonly socketGeneration: number;
  readonly socketId: string;
  readonly processEpoch: string;
  readonly domain: SyncDomain;
  readonly domainGeneration: bigint;
}

export interface SyncV2TerminalState {
  readonly socketGeneration: number;
  readonly socketId: string;
  readonly processEpoch: string;
  readonly domainGeneration: bigint;
  readonly ready: boolean;
}

export type SyncV2Control = Extract<
  FirehoseFrame["frame"],
  {
    case:
      | "viewportAccepted"
      | "viewportRejected"
      | "inputAccepted"
      | "inputRejected"
      | "inputAmbiguous";
  }
>;

// Only the current dial is globally reachable. Every lifecycle resource stays
// on its owning record so late generation-N callbacks cannot mutate N+1.
let _liveLink: LiveSyncLink | null = null;
let _wsGen = 0;
let _smokeTransportPaused = false;
let _resumeSmokeTransport: (() => void) | null = null;

export interface SyncSubscribedState {
  readonly socketGeneration: number;
  readonly socketId: string;
  readonly processEpoch: string;
}

export interface SyncDomainSnapshot {
  readonly apply: () => void;
  readonly snapshotToken?: string;
}

export type SyncDomainHydrator = (
  token: SyncDomainToken,
) => Promise<SyncDomainSnapshot | null>;

const _v2ControlHandlers = new Set<(
  control: SyncV2Control,
  state: SyncV2TerminalState,
) => void>();
const _v2GenerationHandlers = new Set<(state: SyncV2TerminalState | null) => void>();
const _domainHydrationTriggers = new Map<SyncDomain, Set<() => void>>();
const _lazyHydrationTriggers = new Map<SyncDomain, Set<() => void>>();
const _subscribedWaiters = new Set<(state: SyncSubscribedState) => void>();

export function currentSyncDomainToken(domain: SyncDomain): SyncDomainToken | null {
  const link = _liveLink;
  const domainState = link?.v2?.domains.get(domain);
  if (
    !link
    || !link.accepting
    || link.ws.readyState !== WebSocket.OPEN
    || !link.v2
    || !domainState
  ) return null;
  return {
    socketGeneration: link.gen,
    socketId: link.v2.socketId,
    processEpoch: link.v2.processEpoch,
    domain,
    domainGeneration: domainState.generation,
  };
}

export function currentSyncV2TerminalState(): SyncV2TerminalState | null {
  const token = currentSyncDomainToken(SyncDomain.TERMINAL);
  if (!token) return null;
  const ready = _liveLink?.v2?.domains.get(SyncDomain.TERMINAL)?.ready ?? false;
  return {
    socketGeneration: token.socketGeneration,
    socketId: token.socketId,
    processEpoch: token.processEpoch,
    domainGeneration: token.domainGeneration,
    ready,
  };
}

function _notifyV2Generation(state: SyncV2TerminalState | null): void {
  for (const handler of _v2GenerationHandlers) handler(state);
}

export function registerSyncV2GenerationHandler(
  handler: (state: SyncV2TerminalState | null) => void,
): () => void {
  _v2GenerationHandlers.add(handler);
  handler(currentSyncV2TerminalState());
  return () => { _v2GenerationHandlers.delete(handler); };
}

export function registerSyncV2ControlHandler(
  handler: (control: SyncV2Control, state: SyncV2TerminalState) => void,
): () => void {
  _v2ControlHandlers.add(handler);
  return () => { _v2ControlHandlers.delete(handler); };
}

export function sendSyncV2Command(command: SyncClientFrame["command"]): boolean {
  const link = _liveLink;
  if (
    !link
    || !link.v2
    || !link.accepting
    || link.ws.readyState !== WebSocket.OPEN
    || command.case === undefined
  ) return false;
  try {
    link.ws.send(toBinary(SyncClientFrameSchema, create(SyncClientFrameSchema, {
      socketId: link.v2.socketId,
      command,
    })));
    return true;
  } catch {
    return false;
  }
}

export function isCurrentSyncDomainToken(token: SyncDomainToken): boolean {
  const current = currentSyncDomainToken(token.domain);
  return !!current
    && current.socketGeneration === token.socketGeneration
    && current.socketId === token.socketId
    && current.processEpoch === token.processEpoch
    && current.domainGeneration === token.domainGeneration;
}

export function applySyncDomainSnapshot(
  token: SyncDomainToken,
  snapshot: SyncDomainSnapshot,
): boolean {
  if (!isCurrentSyncDomainToken(token)) return false;
  const link = _liveLink!;
  const domain = link.v2!.domains.get(token.domain)!;
  if (!domain.subscribed) return false;
  snapshot.apply();
  markPhase("snapshot_applied", {
    domain: SyncDomain[token.domain],
    generation: token.domainGeneration,
  });
  if (!sendSyncV2Command({
    case: "domainReady",
    value: create(SyncDomainReadyCommandSchema, {
      domain: token.domain,
      generation: token.domainGeneration,
      snapshotToken: snapshot.snapshotToken,
    }),
  })) return false;
  if (!isCurrentSyncDomainToken(token)) return false;
  domain.ready = true;
  if (token.domain === SyncDomain.TERMINAL) {
    setOpen(true);
    _notifyV2Generation(currentSyncV2TerminalState());
  }
  return true;
}

function _triggerDomainHydration(domain: SyncDomain): void {
  for (const trigger of _domainHydrationTriggers.get(domain) ?? []) trigger();
  for (const trigger of _lazyHydrationTriggers.get(domain) ?? []) trigger();
}

function _registerDomainHydrator(
  registry: Map<SyncDomain, Set<() => void>>,
  domain: SyncDomain,
  hydrate: SyncDomainHydrator,
): () => void {
  let disposed = false;
  let lastGenerationKey = "";
  let retryAttempt = 0;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;

  const run = (token: SyncDomainToken, generationKey: string): void => {
    void Promise.resolve().then(() => hydrate(token)).then((snapshot) => {
      markPhase("snapshot_complete", {
        domain: SyncDomain[domain],
        generation: token.domainGeneration,
        status: snapshot ? "fulfilled" : "rejected",
      });
      if (disposed || !isCurrentSyncDomainToken(token)) return;
      if (snapshot && applySyncDomainSnapshot(token, snapshot)) {
        retryAttempt = 0;
        return;
      }
      scheduleRetry(token, generationKey);
    }).catch((error) => {
      markPhase("snapshot_complete", {
        domain: SyncDomain[domain],
        generation: token.domainGeneration,
        status: "rejected",
      });
      diag("sync.snapshot_failed", { domain, error: String(error) });
      scheduleRetry(token, generationKey);
    });
  };

  const scheduleRetry = (token: SyncDomainToken, generationKey: string): void => {
    if (
      disposed
      || retryTimer
      || lastGenerationKey !== generationKey
      || !isCurrentSyncDomainToken(token)
    ) return;
    const state = _liveLink?.v2?.domains.get(domain);
    if (!state?.subscribed || state.ready) return;
    const delay = Math.min(500 * 2 ** retryAttempt, 10_000);
    retryAttempt += 1;
    retryTimer = setTimeout(() => {
      retryTimer = null;
      if (!disposed && isCurrentSyncDomainToken(token)) run(token, generationKey);
    }, delay);
  };

  const trigger = (): void => {
    if (disposed) return;
    const token = currentSyncDomainToken(domain);
    const state = _liveLink?.v2?.domains.get(domain);
    if (!token || !state?.subscribed || state.ready) return;
    const generationKey = `${token.socketId}:${token.domainGeneration}`;
    if (generationKey === lastGenerationKey) return;
    if (retryTimer) {
      clearTimeout(retryTimer);
      retryTimer = null;
    }
    lastGenerationKey = generationKey;
    retryAttempt = 0;
    run(token, generationKey);
  };

  let triggers = registry.get(domain);
  if (!triggers) {
    triggers = new Set();
    registry.set(domain, triggers);
  }
  triggers.add(trigger);
  trigger();
  return () => {
    disposed = true;
    clearTimeout(retryTimer ?? undefined);
    retryTimer = null;
    triggers!.delete(trigger);
    if (triggers!.size === 0) registry.delete(domain);
  };
}

export function registerSyncDomainHydrator(
  domain: SyncDomain,
  hydrate: SyncDomainHydrator,
): () => void {
  return _registerDomainHydrator(_domainHydrationTriggers, domain, hydrate);
}

function _activateLazyDomain(domain: SyncDomain): void {
  const token = currentSyncDomainToken(domain);
  const state = _liveLink?.v2?.domains.get(domain);
  if (!token || !state || state.subscribed) return;
  if (!sendSyncV2Command({
    case: "domainSubscribe",
    value: create(SyncDomainSubscriptionCommandSchema, {
      domain,
      generation: token.domainGeneration,
    }),
  })) return;
  state.subscribed = true;
  state.ready = false;
  _triggerDomainHydration(domain);
}

export function registerLazySyncDomain(
  domain: SyncDomain.AUDIT | SyncDomain.WEBHOOK,
  hydrate: SyncDomainHydrator,
): () => void {
  const first = (_lazyHydrationTriggers.get(domain)?.size ?? 0) === 0;
  const unregister = _registerDomainHydrator(_lazyHydrationTriggers, domain, hydrate);
  if (first) _activateLazyDomain(domain);
  return () => {
    unregister();
    if ((_lazyHydrationTriggers.get(domain)?.size ?? 0) !== 0) return;
    const token = currentSyncDomainToken(domain);
    const state = _liveLink?.v2?.domains.get(domain);
    if (!token || !state?.subscribed) return;
    sendSyncV2Command({
      case: "domainUnsubscribe",
      value: create(SyncDomainSubscriptionCommandSchema, {
        domain,
        generation: token.domainGeneration,
      }),
    });
    state.subscribed = false;
    state.ready = false;
  };
}

export function waitForSyncSubscribed(timeoutMs: number): Promise<SyncSubscribedState | null> {
  const link = _liveLink;
  if (link?.v2) {
    return Promise.resolve({
      socketGeneration: link.gen,
      socketId: link.v2.socketId,
      processEpoch: link.v2.processEpoch,
    });
  }
  const { promise, resolve } = Promise.withResolvers<SyncSubscribedState | null>();
  let settled = false;
  let timer: ReturnType<typeof setTimeout>;
  const accept = (state: SyncSubscribedState): void => {
    if (settled) return;
    settled = true;
    clearTimeout(timer);
    _subscribedWaiters.delete(accept);
    resolve(state);
  };
  _subscribedWaiters.add(accept);
  timer = setTimeout(() => {
    if (settled) return;
    settled = true;
    _subscribedWaiters.delete(accept);
    resolve(null);
  }, timeoutMs);
  return promise;
}

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

function _deactivateV2Link(link: LiveSyncLink): void {
  if (!link.v2) return;
  link.v2 = null;
  if (_liveLink === link) _notifyV2Generation(null);
}

function _cleanupLink(link: LiveSyncLink): void {
  link.accepting = false;
  _deactivateV2Link(link);
  clearTimeout(link.closeEscapeTimer ?? undefined);
  link.closeEscapeTimer = null;
  link.watchdog?.stop();
  link.watchdog = null;
}

function _armCloseEscape(link: LiveSyncLink): void {
  clearTimeout(link.closeEscapeTimer ?? undefined);
  link.closeEscapeTimer = setTimeout(() => {
    link.accepting = false;
    _deactivateV2Link(link);
    link.closeEscapeTimer = null;
    if (_liveLink === link) setOpen(false);
    link.resolveClosed();
  }, WS_CLOSE_ESCAPE_MS);
}

function _initiateWsClose(reason: Exclude<SyncAbortReason, null>): void {
  const link = _liveLink;
  if (!link) return;
  link.accepting = false;
  _deactivateV2Link(link);
  link.abortReason = reason;
  if (_liveLink === link) setOpen(false);
  try { link.ws.close(); } catch { link.resolveClosed(); }
  _armCloseEscape(link);
}

function _closeFailedLink(link: LiveSyncLink): void {
  link.accepting = false;
  _deactivateV2Link(link);
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


// Backward-compatible name for callers migrating from the raw-open barrier.
// V2 resolves only after the subscribed control establishes socket/domain
// generations, never at WebSocket.onopen.
export async function syncSocketOpened(timeoutMs: number): Promise<void> {
  await waitForSyncSubscribed(timeoutMs);
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
function _dispatchSyncFrame(frame: FirehoseFrame): boolean {
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
          case "terminalLink": {
            const link = v as { sessionId: string; text: string; uri: string };
            _dispatchTerminalLink(link.sessionId, link.text, link.uri);
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

const SYNC_V2_DOMAINS = [
  SyncDomain.TERMINAL,
  SyncDomain.WORKERS,
  SyncDomain.WORKSPACES,
  SyncDomain.TASKS,
  SyncDomain.PERMISSIONS,
  SyncDomain.MCP,
  SyncDomain.PAIR,
  SyncDomain.WEBHOOK,
  SyncDomain.AUDIT,
] as const;

function _handleSubscribed(link: LiveSyncLink, subscribed: SyncSubscribedFrame): void {
  if (link.v2 || !subscribed.socketId || !subscribed.processEpoch) {
    throw new Error("duplicate or malformed subscribed control");
  }
  const domains = new Map<SyncDomain, SyncV2DomainState>();
  for (const entry of subscribed.generations) {
    if (
      !SYNC_V2_DOMAINS.includes(entry.domain as (typeof SYNC_V2_DOMAINS)[number])
      || entry.generation <= 0n
      || domains.has(entry.domain)
    ) throw new Error("invalid subscribed domain generation");
    domains.set(entry.domain, {
      generation: entry.generation,
      subscribed: entry.subscribed,
      ready: false,
    });
  }
  if (domains.size !== SYNC_V2_DOMAINS.length) {
    throw new Error("incomplete subscribed domain generations");
  }
  link.v2 = {
    socketId: subscribed.socketId,
    processEpoch: subscribed.processEpoch,
    domains,
    routableChunks: new Map(),
  };
  _noteSyncConnect();
  markPhase("sync_subscribed", {
    generation: link.gen,
    processEpoch: subscribed.processEpoch,
  });
  const state: SyncSubscribedState = {
    socketGeneration: link.gen,
    socketId: subscribed.socketId,
    processEpoch: subscribed.processEpoch,
  };
  for (const resolve of _subscribedWaiters) resolve(state);
  _subscribedWaiters.clear();
  for (const domain of SYNC_V2_DOMAINS) {
    if (_lazyHydrationTriggers.has(domain)) _activateLazyDomain(domain);
    else if (domains.get(domain)?.subscribed) _triggerDomainHydration(domain);
  }
  _notifyV2Generation(currentSyncV2TerminalState());
}

function _handleDomainReset(link: LiveSyncLink, reset: SyncDomainResetFrame): void {
  const v2 = link.v2;
  const domain = v2?.domains.get(reset.domain);
  if (!v2 || !domain || reset.generation <= 0n) {
    throw new Error("invalid domain reset");
  }
  domain.generation = reset.generation;
  domain.subscribed = reset.subscribed;
  domain.ready = false;
  if (reset.domain === SyncDomain.WORKERS) v2.routableChunks.clear();
  if (reset.domain === SyncDomain.TERMINAL) {
    setOpen(false);
    _notifyV2Generation(currentSyncV2TerminalState());
  }
  if (reset.subscribed) _triggerDomainHydration(reset.domain);
}

function _dispatchRoutableChunk(
  link: LiveSyncLink,
  value: WorkerRoutableFrame,
): boolean {
  if (!value.snapshotId) {
    setRoutableFps(new Set(value.fps));
    return true;
  }
  if (
    value.chunkCount <= 0
    || value.chunkCount > 4096
    || value.chunkIndex >= value.chunkCount
  ) return false;
  const chunks = link.v2!.routableChunks;
  let snapshot = chunks.get(value.snapshotId);
  if (!snapshot) {
    chunks.clear();
    snapshot = {
      count: value.chunkCount,
      chunks: new Array<string[] | undefined>(value.chunkCount),
    };
    chunks.set(value.snapshotId, snapshot);
  }
  if (snapshot.count !== value.chunkCount) return false;
  snapshot.chunks[value.chunkIndex] = value.fps;
  if (snapshot.chunks.some((chunk) => chunk === undefined)) return true;
  setRoutableFps(new Set(snapshot.chunks.flatMap((chunk) => chunk!)));
  chunks.delete(value.snapshotId);
  return true;
}

function _dispatchV2Application(link: LiveSyncLink, frame: FirehoseFrame): boolean {
  const v2 = link.v2;
  const domain = v2?.domains.get(frame.domain);
  if (!v2 || !domain || frame.deliverySeq <= 0n) {
    throw new Error("malformed v2 application frame");
  }
  if (frame.domainGeneration !== domain.generation) return true;
  if (!domain.subscribed) return true;
  if (!domain.ready) {
    throw new Error("application frame arrived before domain readiness");
  }
  if (frame.frame.case === "workerRoutable") {
    return _dispatchRoutableChunk(link, frame.frame.value);
  }
  return _dispatchSyncFrame(frame);
}

function _handleV2Control(link: LiveSyncLink, frame: FirehoseFrame): boolean {
  if (
    frame.deliverySeq !== 0n
    || frame.domain !== SyncDomain.UNSPECIFIED
    || frame.domainGeneration !== 0n
  ) throw new Error("sequenced v2 control");
  switch (frame.frame.case) {
    case "subscribed":
      _handleSubscribed(link, frame.frame.value);
      return true;
    case "domainReset":
      _handleDomainReset(link, frame.frame.value);
      return true;
    case "viewportAccepted":
    case "viewportRejected":
    case "inputAccepted":
    case "inputRejected":
    case "inputAmbiguous": {
      const state = currentSyncV2TerminalState();
      if (!state) return true;
      const control = frame.frame as SyncV2Control;
      for (const handler of _v2ControlHandlers) handler(control, state);
      return true;
    }
    case "uiState":
    case "uiCommand":
    case "keepalive":
    case "coordinatorRelocation":
      return _dispatchSyncFrame(frame);
    default:
      return false;
  }
}

function _consumeSyncFrame(link: LiveSyncLink, frame: FirehoseFrame): void {
  try {
    const isControl = frame.deliverySeq === 0n;
    if (link.expectsV2 && !link.v2) {
      if (frame.frame.case !== "subscribed" || !_handleV2Control(link, frame)) {
        throw new Error("application frame arrived before subscribed");
      }
      return;
    }
    if (link.v2 && isControl) {
      if (!_handleV2Control(link, frame)) throw new Error("unknown v2 control");
      return;
    }
    const outcome = dispatchSyncFrameCausally(
      () => _liveLink,
      link,
      WebSocket.OPEN,
      frame,
      link.v2
        ? (accepted) => _dispatchV2Application(link, accepted)
        : _dispatchSyncFrame,
      link.v2?.socketId ?? "",
    );
    if (outcome === "unapplied") throw new Error("unapplied sync frame");
  } catch (error) {
    signal("diag.corruption_signal", {
      kind: "sync_ws_dispatch",
      frame: frame.frame.case ?? "unknown",
      msg: String(error),
      cooldownKey: "sync",
    });
    _closeFailedLink(link);
  }
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
      // flow=1 preserves cumulative application ACKs; exact sync_v=2 opts this
      // build into subscribed/domain controls without exposing them to cached
      // flow=1 clients.
      const url = `${wsBase}/ws/coord-sync?since=${_lastSeenEventId}&tab=${encodeURIComponent(getTabId())}&flow=1&sync_v=2`;
      const ws = new WebSocket(url, ["roost-auth", jwt]);
      ws.binaryType = "arraybuffer";
      const gen = ++_wsGen;
      const { promise: closed, resolve: resolveClosed } = Promise.withResolvers<void>();
      const link: LiveSyncLink = {
        ws,
        gen,
        abortReason: null,
        accepting: false,
        resolveClosed,
        expectsV2: true,
        closeEscapeTimer: null,
        watchdog: null,
        v2: null,
      };
      dialLink = link;
      _liveLink = link;
      ws.onopen = () => {
        if (!canOpenSyncLink(_liveLink, link, WebSocket.OPEN)) {
          link.accepting = false;
          try { ws.close(); } catch { /* obsolete or closing dial */ }
          return;
        }
        link.accepting = true;
        // Raw OPEN is not hydration readiness. The subscribed control installs
        // generations; terminal domain_ready raises syncStreamOpen.
        link.watchdog = startStaleWatchdog(ws, {
          onStale: () => {
            if (_liveLink === link) _initiateWsClose("stale");
          },
        });
      };
      ws.onmessage = (ev) => {
        // This callback is the one link-identity acceptance gate.
        if (!canAcceptSyncLink(_liveLink, link, WebSocket.OPEN)) return;
        let frame: FirehoseFrame;
        try {
          frame = decodeFirehoseFrame(new Uint8Array(ev.data as ArrayBuffer));
        } catch (e) {
          signal("diag.corruption_signal", {
            kind: "sync_ws_decode",
            frame: "firehose",
            msg: String(e),
            cooldownKey: "sync",
          });
          _closeFailedLink(link);
          return;
        }
        _consumeSyncFrame(link, frame);
        if (link.v2) {
          _syncFailures = 0;
          backoff = 1000;
        }
      };
      ws.onerror = (e) => { console.debug("[sync.connect] ws error", e); };
      ws.onclose = (event) => {
        link.accepting = false;
        if (
          link.abortReason === null
          && isSyncBackpressureClose(event.code, event.reason)
        ) link.abortReason = "flow";
        if (_liveLink === link) {
          resetCellMountBuffers();
          setOpen(false);
        }
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
    // Intentional lifecycle closes and server flow recovery redial immediately.
    if (isImmediateSyncRedial(abortReason)) {
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

