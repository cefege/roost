// Adapts worker terminal metadata into coordinator-owned semantic hubs.
// New workers supply WTerminalMetadata; old workers retain WBinary compatibility
// through the same incremental title parser, never exposing PTY bytes to Sync.
// Session close and route loss release every compatibility parser state.

import type { WTerminalMetadata } from "@roost/shared/proto/worker_transport_pb";
import { TerminalTitleParser } from "@roost/shared/terminal-metadata";
import { diag } from "@roost/shared/diag";
import type { ChannelId, WorkerFp } from "@roost/shared/wire";
import { lookupSessionId } from "./byte-hub.ts";
import { sessionBus } from "./buses.ts";
import { observeTerminalActivity } from "./last-activity-hub.ts";
import { observeTerminalTitle } from "./terminal-title-hub.ts";
import { subscribeTerminalRouteRetirement } from "./terminal-route-retirement.ts";

interface LegacyParserEntry {
  sessionId: string;
  parser: TerminalTitleParser;
}

const legacyParsersByChannel = new Map<string, LegacyParserEntry>();
const legacyChannelsBySession = new Map<string, Set<string>>();

/** Consume metadata from a negotiated worker without retaining transport bytes. */
export function acceptTerminalMetadata(
  workerFp: WorkerFp,
  metadata: WTerminalMetadata,
): void {
  const channelId = metadata.channelId as ChannelId;
  const sessionId = lookupSessionId(workerFp, channelId);
  if (!sessionId) {
    diag("terminal_metadata.drop_unmapped", {
      worker_fp: workerFp,
      channel_id: metadata.channelId,
      kind: "semantic",
    });
    return;
  }
  if (metadata.titleChanged) observeTerminalTitle(sessionId, metadata.title);
  if (!metadata.activityChanged) return;
  if (metadata.activityTsMs > BigInt(Number.MAX_SAFE_INTEGER)) {
    diag("terminal_metadata.drop_invalid_activity", {
      worker_fp: workerFp,
      channel_id: metadata.channelId,
    });
    return;
  }
  observeTerminalActivity(sessionId, Number(metadata.activityTsMs));
}

/** Consume old-worker raw output only long enough to derive semantic metadata. */
export function acceptLegacyTerminalMetadata(
  workerFp: WorkerFp,
  channelId: ChannelId,
  bytes: Uint8Array,
): void {
  if (bytes.byteLength === 0) return;
  const sessionId = lookupSessionId(workerFp, channelId);
  const key = `${workerFp}:${channelId}`;
  if (!sessionId) {
    removeLegacyParser(key);
    diag("terminal_metadata.drop_unmapped", {
      worker_fp: workerFp,
      channel_id: channelId,
      kind: "legacy",
    });
    return;
  }
  let entry = legacyParsersByChannel.get(key);
  if (!entry || entry.sessionId !== sessionId) {
    removeLegacyParser(key);
    entry = { sessionId, parser: new TerminalTitleParser() };
    legacyParsersByChannel.set(key, entry);
    let channelKeys = legacyChannelsBySession.get(sessionId);
    if (!channelKeys) {
      channelKeys = new Set();
      legacyChannelsBySession.set(sessionId, channelKeys);
    }
    channelKeys.add(key);
  }
  const title = entry.parser.push(bytes);
  if (title) observeTerminalTitle(sessionId, title.title);
  observeTerminalActivity(sessionId, Date.now());
}

/** Start lifecycle cleanup for parser state retained by old-worker compatibility. */
export function startTerminalMetadataAdapter(): () => void {
  const unsubscribeRouteRetirement = subscribeTerminalRouteRetirement(removeLegacyParser);
  const unsubscribeSessionBus = sessionBus.subscribe((event) => {
    if (event.kind !== "closed") return;
    const channelKeys = legacyChannelsBySession.get(event.session_id);
    if (!channelKeys) return;
    while (channelKeys.size > 0) removeLegacyParser(channelKeys.values().next().value!);
  });
  return () => {
    unsubscribeRouteRetirement();
    unsubscribeSessionBus();
    legacyParsersByChannel.clear();
    legacyChannelsBySession.clear();
  };
}

function removeLegacyParser(key: string): void {
  const entry = legacyParsersByChannel.get(key);
  if (!entry) return;
  legacyParsersByChannel.delete(key);
  const channelKeys = legacyChannelsBySession.get(entry.sessionId);
  if (!channelKeys) return;
  channelKeys.delete(key);
  if (channelKeys.size === 0) legacyChannelsBySession.delete(entry.sessionId);
}
