// Agent-session transcript store — the SPA's mirror of the worker's per-session
// entry ring. Only kind="agent" sessions have one; shells keep the cell grid.
//
// The whole contract is UPSERT BY SEQ. `seq` is monotonic per session and
// starts at 1, and the worker RE-EMITS an entry under the same seq as it grows
// (streaming assistant text, a tool finishing, a prompt being answered). That
// is what makes the live stream idempotent: a reconnect that replays a window,
// or a backfill page overlapping the tail, costs nothing but a replace.
//
// History is NOT on the firehose. AgentEntriesFrame is presence-class —
// coord's bus is a volatile ring with no durable replay — so everything older
// than the live stream arrives through backfillEntries →
// SessionsGetAgentEntries, one 128-entry page per call.
//
// Callers: store/sync.ts (`agentEntries` firehose case),
// components/agent/Transcript.tsx (mount backfill + the "Load earlier" button).

import { createStore, produce, unwrap } from "solid-js/store";
import { agentEntryFromProto } from "@roost/shared/wire/agent-proto";
// Type-only on purpose: wire/agent-entry is NOT in @roost/shared's exports map
// (only ./wire, ./wire/agent-proto, ./wire/event-proto, ./wire/row-proto), so a
// VALUE import would typecheck through the tsconfig path and then fail to
// resolve in the bundler. Types are erased, so this one is safe.
import type { AgentEntry } from "@roost/shared/wire/agent-entry";
import { log } from "@roost/shared/log";
import { coordClient } from "../connect.ts";

interface AgentEntriesState {
  /** key = SessionId. Ascending by `seq`. A gap between the backfilled head and
   *  the live tail is legal — the worker ring drops oldest past 2000 entries. */
  bySession: Record<string, AgentEntry[]>;
  /** key = SessionId. Whether a page exists behind the oldest entry held.
   *  Absent until the first backfill answers, so "Load earlier" stays hidden
   *  rather than flashing on a transcript that has no history at all. */
  hasEarlierBySession: Record<string, boolean>;
  /** key = SessionId. A backfill page is in flight. */
  loadingBySession: Record<string, boolean>;
}

const [agentEntriesStore, setStore] = createStore<AgentEntriesState>({
  bySession: {},
  hasEarlierBySession: {},
  loadingBySession: {},
});

// seq → array index, per session. Deliberately OUTSIDE the store: it is a pure
// derivative of the array that nothing renders, so storing it would buy a proxy
// write per upserted entry and reactivity no consumer subscribes to.
const indexBySession = new Map<string, Map<number, number>>();

// Shared empty tail so a session with no transcript yet hands <Index> the same
// reference every read instead of minting an array per call.
const EMPTY: readonly AgentEntry[] = [];

/** Reactive: this session's transcript, ascending by seq. */
export function agentEntries(sessionId: string): readonly AgentEntry[] {
  return agentEntriesStore.bySession[sessionId] ?? EMPTY;
}

/** Reactive: is there an older page to pull? False until the first page lands. */
export function hasEarlierEntries(sessionId: string): boolean {
  return agentEntriesStore.hasEarlierBySession[sessionId] === true;
}

/** Reactive: a backfill page is in flight for this session. */
export function isBackfilling(sessionId: string): boolean {
  return agentEntriesStore.loadingBySession[sessionId] === true;
}

/** Reactive: has this session's transcript been fetched at least once? Drives
 *  "no messages yet" vs "still loading" without a second flag. */
export function hasBackfilled(sessionId: string): boolean {
  return agentEntriesStore.hasEarlierBySession[sessionId] !== undefined;
}

/**
 * Drop a session's transcript once it closes. Without this a long-lived tab
 * accumulates every agent session it ever watched — up to the worker ring's
 * 2000 entries each, tool text and all — for the life of the tab.
 *
 * Safe on `closed` specifically because a closed session has no transcript
 * view: MainPane renders TranscriptDeck from `activeOpenSession()`, which
 * requires `status === "open"`, and routing to a dead session bounces to the
 * last open one. So this can never wipe entries a user is looking at — which
 * would be unrecoverable, since the worker's controller dies with the session
 * and SessionsGetAgentEntries would answer "unknown session".
 */
export function pruneAgentEntries(sessionId: string): void {
  indexBySession.delete(sessionId);
  if (!unwrap(agentEntriesStore).bySession[sessionId]) return;
  setStore(produce((s: AgentEntriesState) => {
    delete s.bySession[sessionId];
    delete s.hasEarlierBySession[sessionId];
    delete s.loadingBySession[sessionId];
  }));
}

/**
 * Fold a batch of entries in. Matching `seq` replaces in place (the streaming
 * case); a higher `seq` appends (the live case); a lower `seq` splices into
 * order (the backfill case).
 */
export function upsertEntries(sessionId: string, entries: readonly AgentEntry[]): void {
  if (entries.length === 0) return;
  if (!unwrap(agentEntriesStore).bySession[sessionId]) setStore("bySession", sessionId, []);
  const index = ensureIndex(sessionId);
  let spliced = false;
  setStore("bySession", sessionId, produce((list: AgentEntry[]) => {
    for (const e of entries) {
      const last = list.length - 1;
      // Fast path while the seq→index map is still trustworthy: the live
      // stream is monotonic, so an entry either matches a known seq or
      // extends the tail.
      if (!spliced) {
        const at = index.get(e.seq);
        if (at !== undefined) {
          // Same seq, fuller body. Writing the slot (not the array) keeps
          // <Index> from rebuilding the row's DOM — the hot path during a
          // streaming turn.
          list[at] = e;
          continue;
        }
        if (last < 0 || list[last]!.seq < e.seq) {
          index.set(e.seq, list.length);
          list.push(e);
          continue;
        }
      } else if (list[last]!.seq < e.seq) {
        list.push(e);
        continue;
      }
      // Older than the tail (a backfill page landing under the live stream) —
      // or a splice earlier in THIS batch already shifted every index past it,
      // which makes the map a liar for the rest of the loop. Binary-search the
      // slot instead; reindex() repairs the map once the batch is done.
      const lb = lowerBound(list, e.seq);
      if (list[lb]?.seq === e.seq) {
        list[lb] = e;
        continue;
      }
      list.splice(lb, 0, e);
      spliced = true;
    }
  }));
  if (spliced) reindex(sessionId);
}

/**
 * Pull ONE older page: `before_seq` = the lowest seq held, 0 on first load
 * ("newest page"). Concurrent calls for the same session share the in-flight
 * promise — a mount racing the "Load earlier" button must not fire two RPCs
 * against the same cursor and double-insert nothing while looking busy twice.
 */
export function backfillEntries(sessionId: string): Promise<void> {
  const running = inFlight.get(sessionId);
  if (running) return running;
  setStore("loadingBySession", sessionId, true);
  const p = fetchPage(sessionId)
    .catch((err: unknown) => {
      // Coord answers a worker timeout with Unavailable rather than an empty
      // page (empty reads as "no history"), so a failure must leave both
      // hasEarlier and the entries untouched — retrying is safe and hits the
      // same cursor. A mid-scroll failure keeps "Load earlier" offered (a prior
      // page already set hasEarlier); a FIRST-page failure has no such flag, so
      // Transcript offers its own Retry off hasBackfilled/isBackfilling.
      log.warn("agentEntries", "backfill failed", { session_id: sessionId, error: String(err) });
    })
    .finally(() => {
      inFlight.delete(sessionId);
      setStore("loadingBySession", sessionId, false);
    });
  inFlight.set(sessionId, p);
  return p;
}

const inFlight = new Map<string, Promise<void>>();

async function fetchPage(sessionId: string): Promise<void> {
  const list = unwrap(agentEntriesStore).bySession[sessionId];
  const beforeSeq = list && list.length > 0 ? BigInt(list[0]!.seq) : 0n;
  const res = await coordClient.sessionsGetAgentEntries({ sessionId, beforeSeq });
  upsertEntries(sessionId, res.entries.map(agentEntryFromProto));
  setStore("hasEarlierBySession", sessionId, res.more);
}

function ensureIndex(sessionId: string): Map<number, number> {
  let index = indexBySession.get(sessionId);
  if (!index) {
    index = new Map<number, number>();
    indexBySession.set(sessionId, index);
  }
  return index;
}

// First index whose entry.seq >= seq. Only reached on an out-of-order insert;
// the live stream is monotonic and takes the append branch above.
function lowerBound(list: readonly AgentEntry[], seq: number): number {
  let lo = 0;
  let hi = list.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (list[mid]!.seq < seq) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

function reindex(sessionId: string): void {
  const list = unwrap(agentEntriesStore).bySession[sessionId];
  const index = ensureIndex(sessionId);
  index.clear();
  if (!list) return;
  for (let i = 0; i < list.length; i++) index.set(list[i]!.seq, i);
}
