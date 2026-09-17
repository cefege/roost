// Each Sync domain must hydrate exactly once for its current generation before deltas apply.
// This module owns hydrator registration, bounded snapshot retries, and lazy audit membership.
// It depends on sync-domain-state for guarded publication and sync-link-state for token identity.
// Inbound controls only trigger this registry; they never publish snapshots themselves.

import { create } from "@bufbuild/protobuf";
import {
  SyncDomain,
  SyncDomainSubscriptionCommandSchema,
} from "@roost/shared/proto/sync_pb";
import { diag, signal } from "@roost/shared/diag";
import { markPhase } from "../lib/diag.ts";
import {
  _currentLiveSyncLink,
  currentSyncDomainToken,
  isCurrentSyncDomainToken,
  type SyncDomainToken,
} from "./sync-link-state.ts";
import {
  applySyncDomainSnapshot,
  sendSyncV2Command,
  type SyncDomainHydrator,
} from "./sync-domain-state.ts";

const domainHydrationTriggers = new Map<SyncDomain, Set<() => void>>();
const lazyHydrationTriggers = new Map<SyncDomain, Set<() => void>>();

/** A hydration that never settles holds its domain un-ready forever: no
 * publication target, so a mounted terminal view can never publish, never get
 * an ACCEPTED stream, and never accept a frame again. The deadline converts
 * that silence into the ordinary rejected-snapshot retry. */
export const SYNC_HYDRATION_DEADLINE_MS = 15_000;

export function registerSyncDomainHydrator(
  domain: SyncDomain,
  hydrate: SyncDomainHydrator,
): () => void {
  return registerDomainHydrator(domainHydrationTriggers, domain, hydrate);
}

export function registerLazySyncDomain(
  domain: SyncDomain.AUDIT,
  hydrate: SyncDomainHydrator,
): () => void {
  const first = (lazyHydrationTriggers.get(domain)?.size ?? 0) === 0;
  const unregister = registerDomainHydrator(lazyHydrationTriggers, domain, hydrate);
  if (first) _activateLazySyncDomain(domain);
  return () => {
    unregister();
    if ((lazyHydrationTriggers.get(domain)?.size ?? 0) !== 0) return;
    const token = currentSyncDomainToken(domain);
    const state = _currentLiveSyncLink()?.v2?.domains.get(domain);
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

export function _triggerSyncDomainHydration(domain: SyncDomain): void {
  for (const trigger of domainHydrationTriggers.get(domain) ?? []) trigger();
  for (const trigger of lazyHydrationTriggers.get(domain) ?? []) trigger();
}

export function _activateLazySyncDomain(domain: SyncDomain): void {
  const token = currentSyncDomainToken(domain);
  const state = _currentLiveSyncLink()?.v2?.domains.get(domain);
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
  _triggerSyncDomainHydration(domain);
}

export function _hasLazySyncDomainHydrator(domain: SyncDomain): boolean {
  return lazyHydrationTriggers.has(domain);
}

function registerDomainHydrator(
  registry: Map<SyncDomain, Set<() => void>>,
  domain: SyncDomain,
  hydrate: SyncDomainHydrator,
): () => void {
  let disposed = false;
  let lastGenerationKey = "";
  let retryAttempt = 0;
  let retryTimer: Timer | null = null;

  const run = (token: SyncDomainToken, generationKey: string): void => {
    const controller = new AbortController();
    const { promise: deadline, reject: failDeadline } = Promise.withResolvers<never>();
    const timer = setTimeout(() => {
      signal("sync.hydration_timeout", {
        domain: SyncDomain[domain],
        ms: SYNC_HYDRATION_DEADLINE_MS,
        cooldownKey: SyncDomain[domain],
      });
      controller.abort();
      failDeadline(new Error(`sync domain hydration exceeded ${SYNC_HYDRATION_DEADLINE_MS}ms`));
    }, SYNC_HYDRATION_DEADLINE_MS);
    void Promise.race([
      Promise.resolve().then(() => hydrate(token, { signal: controller.signal })),
      deadline,
    ]).then((snapshot) => {
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
    }).finally(() => { clearTimeout(timer); });
  };

  const scheduleRetry = (token: SyncDomainToken, generationKey: string): void => {
    if (
      disposed
      || retryTimer
      || lastGenerationKey !== generationKey
      || !isCurrentSyncDomainToken(token)
    ) return;
    const state = _currentLiveSyncLink()?.v2?.domains.get(domain);
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
    const state = _currentLiveSyncLink()?.v2?.domains.get(domain);
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
