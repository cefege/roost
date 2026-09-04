// Sync v2 control frames must establish generations before any application delta is accepted.
// This module validates that ordering, routes domain resets, and ACKs only the current live link.
// The websocket loop supplies bytes; domain leaves supply hydration and guarded snapshot state.
// A malformed or unapplied frame closes its generation so reconnect can restore a clean baseline.

import { SyncDomain } from "@roost/shared/proto/sync_pb";
import type {
  FirehoseFrame,
  SyncDomainResetFrame,
  SyncSubscribedFrame,
  WorkerRoutableFrame,
} from "@roost/shared/proto/sync_pb";
import { signal } from "@roost/shared/diag";
import { markPhase } from "../lib/diag.ts";
import {
  dispatchSyncFrameCausally,
} from "./sync-flow.ts";
import { setRoutableFps } from "./sync-routable.ts";
import { _noteSyncConnect } from "./sync-handlers.ts";
import { _dispatchSyncFrame } from "./sync-frame.ts";
import {
  _activateLazySyncDomain,
  _hasLazySyncDomainHydrator,
  _triggerSyncDomainHydration,
} from "./sync-domain-hydration.ts";
import {
  _dispatchSyncV2Control,
  _resolveSyncSubscribedWaiters,
  type SyncSubscribedState,
  type SyncV2Control,
} from "./sync-domain-state.ts";
import {
  _closeFailedSyncLink,
  _currentLiveSyncLink,
  _notifySyncV2Generation,
  currentSyncV2TerminalState,
  type LiveSyncLink,
  type SyncV2DomainState,
} from "./sync-link-state.ts";

const SYNC_V2_DOMAINS = [
  SyncDomain.TERMINAL,
  SyncDomain.WORKERS,
  SyncDomain.WORKSPACES,
  SyncDomain.TASKS,
  SyncDomain.MCP,
  SyncDomain.PAIR,
  SyncDomain.AUDIT,
] as const;

export function _consumeSyncFrame(link: LiveSyncLink, frame: FirehoseFrame): void {
  try {
    const isControl = frame.deliverySeq === 0n;
    if (link.expectsV2 && !link.v2) {
      if (frame.frame.case !== "subscribed" || !handleV2Control(link, frame)) {
        throw new Error("application frame arrived before subscribed");
      }
      return;
    }
    if (link.v2 && isControl) {
      if (!handleV2Control(link, frame)) throw new Error("unknown v2 control");
      return;
    }
    const outcome = dispatchSyncFrameCausally(
      _currentLiveSyncLink,
      link,
      WebSocket.OPEN,
      frame,
      link.v2
        ? (accepted) => dispatchV2Application(link, accepted)
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
    _closeFailedSyncLink(link);
  }
}

function handleSubscribed(link: LiveSyncLink, subscribed: SyncSubscribedFrame): void {
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
  _resolveSyncSubscribedWaiters(state);
  for (const domain of SYNC_V2_DOMAINS) {
    if (_hasLazySyncDomainHydrator(domain)) _activateLazySyncDomain(domain);
    else if (domains.get(domain)?.subscribed) _triggerSyncDomainHydration(domain);
  }
  _notifySyncV2Generation(currentSyncV2TerminalState());
}

function handleDomainReset(link: LiveSyncLink, reset: SyncDomainResetFrame): void {
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
    _notifySyncV2Generation(currentSyncV2TerminalState());
  }
  if (reset.subscribed) _triggerSyncDomainHydration(reset.domain);
}

function dispatchRoutableChunk(
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

function dispatchV2Application(link: LiveSyncLink, frame: FirehoseFrame): boolean {
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
    return dispatchRoutableChunk(link, frame.frame.value);
  }
  return _dispatchSyncFrame(
    frame,
    frame.domain === SyncDomain.TERMINAL
      ? {
        socketGeneration: link.gen,
        socketId: v2.socketId,
        processEpoch: v2.processEpoch,
        domainGeneration: domain.generation,
      }
      : null,
  );
}

function handleV2Control(link: LiveSyncLink, frame: FirehoseFrame): boolean {
  if (
    frame.deliverySeq !== 0n
    || frame.domain !== SyncDomain.UNSPECIFIED
    || frame.domainGeneration !== 0n
  ) throw new Error("sequenced v2 control");
  switch (frame.frame.case) {
    case "subscribed":
      handleSubscribed(link, frame.frame.value);
      return true;
    case "domainReset":
      handleDomainReset(link, frame.frame.value);
      return true;
    case "inputAccepted":
    case "inputRejected":
    case "inputAmbiguous": {
      const state = currentSyncV2TerminalState();
      if (!state) return true;
      _dispatchSyncV2Control(frame.frame as SyncV2Control, state);
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
