// Owns bounded, TTL-scoped browser UI reports for one coordinator composition.
// UI handlers admit canonical reports; Sync feeds and list handlers read snapshots.
// New identities are rate/cardinality limited while existing tab heartbeats bypass
// those admission budgets and remain updatable until the owner is disposed.

import type { UiReportStateRequest } from "@roost/shared/proto/sync_pb";
import {
  UI_STATE_IDENTITY_WINDOW_MS,
  UI_STATE_MAX_TABS_PER_DASHBOARD,
  UI_STATE_MAX_TABS_PER_FINGERPRINT,
  UI_STATE_NEW_IDENTITIES_PER_WINDOW,
} from "@roost/shared/ui-state";
import { log } from "@roost/shared/log";
import { RateLimiter } from "../middleware/rate-limit.ts";

export const UI_STATE_TTL_MS = 5 * 60_000;
const UI_STATE_REAP_INTERVAL_MS = 60_000;
const UI_STATE_RATE_GROUP = "ui-state-new-identity";

export interface UiTabEntry {
  readonly dashboardId: string;
  readonly fp: string;
  readonly tabId: string;
  lastMs: number;
  state: UiReportStateRequest;
}

export interface UiStateOwnerOptions {
  readonly now?: () => number;
  readonly reapIntervalMs?: number | null;
  readonly maxTabsPerFingerprint?: number;
  readonly maxTabsPerDashboard?: number;
  readonly newIdentitiesPerWindow?: number;
  readonly identityWindowMs?: number;
}

export class UiStateCapacityError extends Error {
  constructor() {
    super("ui state identity capacity exhausted");
    this.name = "UiStateCapacityError";
  }
}

export class UiStateIdentityRateError extends Error {
  constructor() {
    super("ui state identity rate exceeded");
    this.name = "UiStateIdentityRateError";
  }
}

export class UiStateOwner {
  /** Explicit diagnostics/test seam; production callers use report/list/snapshot. */
  readonly _statesByTab = new Map<string, UiTabEntry>();

  private readonly now: () => number;
  private readonly maxTabsPerFingerprint: number;
  private readonly maxTabsPerDashboard: number;
  private readonly newIdentitiesPerWindow: number;
  private readonly identityWindowMs: number;
  private readonly identityRateLimiter: RateLimiter;
  private reapTimer: Timer | null = null;

  constructor(options: UiStateOwnerOptions = {}) {
    this.now = options.now ?? Date.now;
    this.maxTabsPerFingerprint = options.maxTabsPerFingerprint
      ?? UI_STATE_MAX_TABS_PER_FINGERPRINT;
    this.maxTabsPerDashboard = options.maxTabsPerDashboard
      ?? UI_STATE_MAX_TABS_PER_DASHBOARD;
    this.newIdentitiesPerWindow = options.newIdentitiesPerWindow
      ?? UI_STATE_NEW_IDENTITIES_PER_WINDOW;
    this.identityWindowMs = options.identityWindowMs ?? UI_STATE_IDENTITY_WINDOW_MS;
    requirePositiveSafeInteger(this.maxTabsPerFingerprint, "per-fingerprint tab capacity");
    requirePositiveSafeInteger(this.maxTabsPerDashboard, "per-dashboard tab capacity");
    requirePositiveSafeInteger(this.newIdentitiesPerWindow, "new identity rate");
    requirePositiveSafeInteger(this.identityWindowMs, "identity rate window");
    this.identityRateLimiter = new RateLimiter({
      now: this.now,
      onReject: ({ key, capacity }) => {
        log.warn("ui-state", "identity_rate_limited", {
          identity_scope: key,
          limiter_capacity: capacity,
        });
      },
    });
    const reapIntervalMs = options.reapIntervalMs === undefined
      ? UI_STATE_REAP_INTERVAL_MS
      : options.reapIntervalMs;
    if (reapIntervalMs !== null) {
      requirePositiveSafeInteger(reapIntervalMs, "UI state reap interval");
      this.reapTimer = setInterval(() => this.reap(this.now()), reapIntervalMs);
      this.reapTimer.unref?.();
    }
  }

  report(input: {
    readonly dashboardId: string;
    readonly fingerprint: string;
    readonly tabId: string;
    readonly state: UiReportStateRequest;
  }): void {
    const now = this.now();
    this.reap(now);
    const key = JSON.stringify([input.dashboardId, input.fingerprint, input.tabId]);
    const existing = this._statesByTab.get(key);
    if (existing) {
      existing.lastMs = now;
      existing.state = input.state;
      return;
    }
    // A device cannot multiply its tab/rate budgets by joining more dashboards;
    // the independent dashboard cap bounds aggregate viewers from all devices.
    let dashboardTabs = 0;
    let fingerprintTabs = 0;
    for (const entry of this._statesByTab.values()) {
      if (entry.dashboardId === input.dashboardId) dashboardTabs++;
      if (entry.fp === input.fingerprint) fingerprintTabs++;
    }
    if (
      dashboardTabs >= this.maxTabsPerDashboard
      || fingerprintTabs >= this.maxTabsPerFingerprint
    ) {
      throw new UiStateCapacityError();
    }

    const identityScope = input.fingerprint;
    const rate = this.identityRateLimiter.consume(
      identityScope,
      UI_STATE_RATE_GROUP,
      this.newIdentitiesPerWindow,
      this.identityWindowMs,
    );
    if (!rate.allowed) throw new UiStateIdentityRateError();

    this._statesByTab.set(key, {
      dashboardId: input.dashboardId,
      fp: input.fingerprint,
      tabId: input.tabId,
      lastMs: now,
      state: input.state,
    });
  }

  list(dashboardId: string): UiTabEntry[] {
    this.reap(this.now());
    return [...this._statesByTab.values()]
      .filter((entry) => entry.dashboardId === dashboardId);
  }

  snapshot(
    dashboardId: string,
  ): Array<{ fp: string; tabId: string; state: UiReportStateRequest }> {
    return this.list(dashboardId).map((entry) => ({
      fp: entry.fp,
      tabId: entry.tabId,
      state: entry.state,
    }));
  }

  reap(now = this.now()): void {
    for (const [key, entry] of this._statesByTab) {
      if (now - entry.lastMs > UI_STATE_TTL_MS) this._statesByTab.delete(key);
    }
  }

  dispose(): void {
    clearInterval(this.reapTimer ?? undefined);
    this.reapTimer = null;
    this._statesByTab.clear();
  }
}

function requirePositiveSafeInteger(value: number, field: string): void {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new RangeError(`${field} must be a positive safe integer`);
  }
}
