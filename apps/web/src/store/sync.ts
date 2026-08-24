// Sync transport: the one long-lived WebSocket to coord's /ws/coord-sync,
// carrying the Sync-v2 firehose (state snapshots, cell grids, per-domain
// deltas). This module owns the DIAL LIFECYCLE ONLY — capped-backoff redial
// while visible, park-and-wake while hidden (any page-lifecycle resume
// re-dials in place), and v2 domain subscribe/hydration bookkeeping. The
// leaf modules hold the rest: sync-frame.ts dispatches each frame,
// sync-handlers.ts folds deltas, sync-bootstrap.ts hydrates before/at
// subscribe, sync-watchdog.ts owns backoff + staleness policy.

import { create, toBinary } from "@bufbuild/protobuf";
import {
  SyncClientFrameSchema,
  SyncDomainReadyCommandSchema,
  SyncDomainSubscriptionCommandSchema,
  SyncDomain,
  type SyncClientFrame,
  type SyncSubscribedFrame,
  type SyncDomainResetFrame,
  type WorkerRoutableFrame,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";
import { signCoordinatorJwt } from "../auth/web-key.ts";
import { getTabId } from "../auth/tab-id.ts";
import { markPhase } from "../lib/diag.ts";
import { isPageVisible } from "../lib/pageVisible.ts";
// tRPC client retired — queries/RPCs route through coordClient (Connect).
// The event firehose is _runConnectSync: a raw WebSocket to coord's
// /ws/coord-sync carrying state, cell grids, and compact terminal metadata.
import { signal, diag } from "@roost/shared/diag";
import {
  SYNC_AUTH_SUBPROTOCOL,
  SYNC_QUERY_FLOW_V1,
  SYNC_QUERY_V2,
  SYNC_WS_PATH,
} from "@roost/shared/wire/sync-ws";
import {
  nextRedialDelayMs,
  shouldCloseStaleLinkOnResume,
  shouldParkRedial,
  startStaleWatchdog,
  SYNC_HIDDEN_PARK_FAILURES,
  SYNC_REDIAL_BASE_MS,
  type StaleWatchdog,
  type SyncLinkLiveness,
} from "./sync-watchdog.ts";
import {
  canAcceptSyncLink,
  canOpenSyncLink,
  decodeFirehoseFrame,
  dispatchSyncFrameCausally,
  isImmediateSyncRedial,
  isSyncBackpressureClose,
} from "./sync-flow.ts";
// Worker-routability signal lives in sync-routable.ts (leaf): _runConnectSync
// writes it; the UI reads workerOnline (re-exported here so consumers keep
// importing it from store/sync.ts).
import { setRoutableFps } from "./sync-routable.ts";
export { workerOnline } from "./sync-routable.ts";
// Presence metadata remains a leaf dispatch. Terminal screen/state ownership is
// in terminal-stream.ts and is re-exported here for diagnostics/smoke callers.
export { registerPresenceHandler } from "./sync-dispatch.ts";
export {
  cellFrameCount,
  cellFullFrameCount,
  lastFullFrameSbRows,
  cellGridEpoch,
} from "./terminal-stream.ts";
// Per-domain delta handlers + keeper-death detector + delta-sub registries
// live in sync-handlers.ts; the per-frame switch calls them and iterates the
// sub Sets.
import { _noteSyncConnect } from "./sync-handlers.ts";
export { registerWebhookDelta, registerAuditDelta } from "./sync-handlers.ts";
// The per-frame switch over every wire-frame kind, and the last-seen event id
// the reconnect backfill resumes from, live in sync-frame.ts.
import { _dispatchSyncFrame, lastSeenSyncEventId } from "./sync-frame.ts";
// Smoke + manual-recovery backdoors live in sync-smoke.ts, re-exported here so
// lib/smoke.ts, sync-bootstrap.ts and the redial tests keep one import site.
export {
  forceSyncMaxBackoff,
  forceSyncReconnect,
  pauseSyncTransport,
  resumeSyncTransport,
} from "./sync-smoke.ts";

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

/** Smoke-only transport gate, driven from store/sync-smoke.ts. Setting it holds
 * the next dial at _runConnectSync's park; clearing it wakes whatever resolver
 * the loop parked on. */
export function _setSmokeTransportPaused(paused: boolean): void {
  _smokeTransportPaused = paused;
  if (paused) return;
  _resumeSmokeTransport?.();
  _resumeSmokeTransport = null;
}

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
function _syncLinkIdleMs(): number {
  const link = _liveLink;
  return link && link.ws.readyState === WebSocket.OPEN && link.watchdog
    ? link.watchdog.idleMs()
    : Number.POSITIVE_INFINITY;
}

/** Is a Sync socket live, dialing, or absent? A dial in flight is already the
 *  redial, so the resume path must never close it. */
function _syncLinkLiveness(): SyncLinkLiveness {
  const link = _liveLink;
  if (!link) return "none";
  return link.accepting && link.ws.readyState === WebSocket.OPEN ? "open" : "dialing";
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
    link.resolveClosed();
  }, WS_CLOSE_ESCAPE_MS);
}

function _initiateWsClose(reason: Exclude<SyncAbortReason, null>): void {
  const link = _liveLink;
  if (!link) return;
  link.accepting = false;
  _deactivateV2Link(link);
  link.abortReason = reason;
  try { link.ws.close(); } catch { link.resolveClosed(); }
  _armCloseEscape(link);
}

/** Close the live tube as an intentional lifecycle close, so the redial loop
 * dials again at once. The smoke seam reaches _initiateWsClose only through this. */
export function _requestSyncRedial(): void {
  _initiateWsClose("manual");
}

function _closeFailedLink(link: LiveSyncLink): void {
  link.accepting = false;
  _deactivateV2Link(link);
  try { link.ws.close(); } catch { link.resolveClosed(); }
  _armCloseEscape(link);
}

// Redial state. A visible document is never left with no live socket AND no
// scheduled redial: the DELAY is capped (sync-watchdog::nextRedialDelayMs), the
// attempt count never is. Only a hidden document parks, and every page-lifecycle
// resume wakes it in place, preserving the mounted terminal deck and its last
// painted grid — the retired eight-failure park could only be cleared by a
// reload.
let _syncFailures = 0;
let _redialDelayMs = SYNC_REDIAL_BASE_MS;
let _hiddenParked = false;
let _wakeHiddenPark: (() => void) | null = null;

/** Smoke-only: pre-arm the redial ladder at the highest floor production can
 * still reach, then drop the tube as a failure. Declared here beside the redial
 * state it mutates; the roostSmoke gate is in store/sync-smoke.ts. */
export function _armSyncRedialFloor(): void {
  _syncFailures = SYNC_HIDDEN_PARK_FAILURES - 1;
  _redialDelayMs = nextRedialDelayMs(SYNC_HIDDEN_PARK_FAILURES);
  const link = _liveLink;
  if (link) _closeFailedLink(link);
}

export interface SyncRedialStatus {
  /** Consecutive failed dials since this tab last received a Sync frame. */
  readonly failures: number;
  /** Delay the pending redial waits — capped, never unbounded. */
  readonly nextDelayMs: number;
  /** True only while a HIDDEN document sleeps instead of redialing. */
  readonly hiddenParked: boolean;
  /** Whether this tab currently has an open socket, a dial in flight, or none. */
  readonly liveness: SyncLinkLiveness;
}

/** Redial status for diagnostics and the smoke seam. `hiddenParked: false` with
 *  no live link means a redial is scheduled, never that the loop stopped. */
export function syncRedialStatus(): SyncRedialStatus {
  return {
    failures: _syncFailures,
    nextDelayMs: _redialDelayMs,
    hiddenParked: _hiddenParked,
    liveness: _syncLinkLiveness(),
  };
}

/** Manually recover the existing Sync loop and replace any live stale socket
 *  without remounting the SPA or its persistent terminal deck. */
export function reconnectNow(): void {
  console.info("[sync.connect] manual reconnect requested");
  resumeSyncNow();
  _initiateWsClose("manual");
}

// Immediate re-dial request. Read by whichever side sees it first, so it works
// whether the reconnect loop is already parked in its backoff sleep or has not
// reached it yet.
let _resumeRequested = false;
let _wakeBackoff: (() => void) | null = null;

/** Re-dial NOW after authorization, a page-lifecycle resume, or an explicit
 * reconnect. Wakes both a hidden park and an ordinary redial backoff. */
export function resumeSyncNow(): void {
  _hiddenParked = false;
  _syncFailures = 0;
  _redialDelayMs = SYNC_REDIAL_BASE_MS;
  _resumeRequested = true;
  const wakePark = _wakeHiddenPark;
  _wakeHiddenPark = null;
  wakePark?.();
  const wakeBackoff = _wakeBackoff;
  _wakeBackoff = null;
  wakeBackoff?.();
}

// Page-lifecycle resume. A frozen, backgrounded, or back/forward-cached tab
// comes back WITHOUT re-running module init: the socket it left behind may be
// half-open and the redial loop may be parked, which is how a tab used to
// "come back dead" until a manual reload. Every resume edge wakes the transport
// in place: visibilitychange→visible, pageshow (bfcache restore included),
// Page Lifecycle resume, and window focus. One restore fires several of those,
// so a monotonic window collapses the burst into ONE wake — a second wake would
// pay for another JWT sign, handshake and backfill, and could race two socket
// generations onto the same tab. Replay is not a second path: the one new
// generation the loop dials notifies registerSyncV2GenerationHandler, which is
// where mounted terminal owners reconcile.
const RESUME_COALESCE_MS = 500;
let _lastResumeAt = Number.NEGATIVE_INFINITY;

export function installSyncLifecycleWake(onResume: () => void): () => void {
  if (typeof document === "undefined" || typeof window === "undefined") {
    return () => { /* non-DOM host: nothing to resume */ };
  }
  const wake = (): void => {
    // A hidden pageshow/focus has nothing to wake, and must not consume the
    // coalesce window the following visibilitychange needs.
    if (!isPageVisible()) return;
    const now = performance.now();
    if (now - _lastResumeAt < RESUME_COALESCE_MS) return;
    _lastResumeAt = now;
    onResume();
    resumeSyncNow();
    // Keep a live socket across a tab switch: re-dialing costs a JWT sign, a
    // TLS handshake and the since= event backfill, all ahead of the terminal's
    // reveal snapshot. Close only a socket that has actually gone silent.
    if (shouldCloseStaleLinkOnResume(_syncLinkLiveness(), _syncLinkIdleMs())) {
      _initiateWsClose("visibility");
    }
  };
  document.addEventListener("visibilitychange", wake);
  document.addEventListener("resume", wake);
  window.addEventListener("pageshow", wake);
  window.addEventListener("focus", wake);
  return () => {
    document.removeEventListener("visibilitychange", wake);
    document.removeEventListener("resume", wake);
    window.removeEventListener("pageshow", wake);
    window.removeEventListener("focus", wake);
  };
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
// only the tube changed; sync-frame.ts's _dispatchSyncFrame is shared verbatim.
export async function _runConnectSync(): Promise<void> {
  while (true) {
    if (_hiddenParked) {
      // Hidden document, unreachable coordinator: sleeping beats dialing on a
      // throttled timer. Parked HERE rather than where the flag is set so a
      // resume landing in between is never lost — any page-lifecycle wake or
      // resumeSyncNow() clears it and re-dials at once.
      console.warn("[sync.connect] hidden and unreachable — sleeping until resume");
      const { promise: resumed, resolve } = Promise.withResolvers<void>();
      _wakeHiddenPark = resolve;
      await resumed;
      if (_wakeHiddenPark === resolve) _wakeHiddenPark = null;
    }
    if (_smokeTransportPaused) {
      const { promise: resumed, resolve } = Promise.withResolvers<void>();
      _resumeSmokeTransport = resolve;
      await resumed;
      if (_resumeSmokeTransport === resolve) _resumeSmokeTransport = null;
    }
    if (_resumeRequested) {
      _resumeRequested = false;
      _redialDelayMs = SYNC_REDIAL_BASE_MS;
    }
    let dialLink: LiveSyncLink | null = null;
    let abortReason: SyncAbortReason = null;
    try {
      console.debug("[sync.connect] starting Sync stream", { sinceEventId: lastSeenSyncEventId(), attempt: _syncFailures + 1 });
      // Coord base: localStorage override for multi-coord testing, else
      // same-origin. http→ws / https→wss.
      const override = typeof localStorage !== "undefined" ? localStorage.getItem("roost.coordinatorUrl") : null;
      const wsBase = (override || location.origin).replace(/^http/, "ws");
      const jwt = await signCoordinatorJwt();
      // flow=v1 preserves cumulative application ACKs; exact sync_v=v2 opts
      // this build into subscribed/domain controls without exposing them to
      // cached flow=v1 clients. Path, query values and subprotocol are wire
      // contract shared with coord's upgrade handler — never inline literals.
      // The JWT travels as a subprotocol so proxies never log it in the URL.
      const url = `${wsBase}${SYNC_WS_PATH}?since=${lastSeenSyncEventId()}&tab=${encodeURIComponent(getTabId())}&flow=${SYNC_QUERY_FLOW_V1}&sync_v=${SYNC_QUERY_V2}`;
      const ws = new WebSocket(url, [SYNC_AUTH_SUBPROTOCOL, jwt]);
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
        // generations; terminal domain_ready publishes the hydrated generation.
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
          _redialDelayMs = SYNC_REDIAL_BASE_MS;
        }
      };
      ws.onerror = (e) => { console.debug("[sync.connect] ws error", e); };
      ws.onclose = (event) => {
        link.accepting = false;
        if (
          link.abortReason === null
          && isSyncBackpressureClose(event.code, event.reason)
        ) link.abortReason = "flow";
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
        if (_liveLink === dialLink) _liveLink = null;
      }
    }
    // Intentional lifecycle closes and server flow recovery redial immediately.
    if (isImmediateSyncRedial(abortReason)) {
      _redialDelayMs = SYNC_REDIAL_BASE_MS;
      continue;
    }
    _syncFailures += 1;
    _redialDelayMs = nextRedialDelayMs(_syncFailures);
    const parking = shouldParkRedial(_syncFailures);
    if (_syncFailures === SYNC_HIDDEN_PARK_FAILURES) {
      // Status, not a stopped loop: the digest still sees a tab that cannot
      // reach its coordinator while the capped redial keeps running.
      console.warn(`[sync.connect] ${_syncFailures} consecutive failures`, { parking });
      signal("reconnect.give_up", {
        failures: _syncFailures,
        action: parking ? "hidden_park" : "keep_retrying",
        cooldownKey: "sync",
      });
    }
    if (parking) { _hiddenParked = true; continue; }
    // resumeSyncNow may land before the sleep starts or while it is parked;
    // both are checked so neither ordering loses the wake.
    if (_resumeRequested) { _resumeRequested = false; _redialDelayMs = SYNC_REDIAL_BASE_MS; continue; }
    const { promise: slept, resolve: wake } = Promise.withResolvers<void>();
    _wakeBackoff = wake;
    const sleepTimer = setTimeout(wake, _redialDelayMs);
    await slept;
    clearTimeout(sleepTimer);
    _wakeBackoff = null;
    if (_resumeRequested) { _resumeRequested = false; _redialDelayMs = SYNC_REDIAL_BASE_MS; }
  }
}
