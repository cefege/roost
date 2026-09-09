// Owns live writable tab-bound Sync-v2 targets and their acknowledged layout applies.
// The UI RPC registers each pending correlation before publishing, while Sync ingress
// settles it only through the exact fingerprint, tab, and socket generation.
// Explicit count maps bound distinct live targets globally and per fingerprint.

import { randomUUID } from "node:crypto";
import { log } from "@roost/shared/log";
import {
  UI_STATE_MAX_TABS_TOTAL,
  UI_STATE_MAX_TABS_PER_FINGERPRINT,
} from "@roost/shared/ui-state";
import {
  UiApplyLayoutOutcome,
  type UiApplyLayoutResult,
} from "@roost/shared/proto/sync_pb";

export const UI_LAYOUT_APPLY_TIMEOUT_MS = 15_000;
export const UI_LAYOUT_APPLY_MAX_PENDING = 256;
export const UI_LAYOUT_REJECTED_REASON_MAX_LENGTH = 200;
export const UI_LAYOUT_TARGET_GONE_REASON = "target acknowledgement unavailable";
const UI_LAYOUT_REJECTED_REASON_FALLBACK = "layout apply rejected";
const UI_LAYOUT_REJECTED_REASON_MAX_INSPECTED_CODE_POINTS =
  UI_LAYOUT_REJECTED_REASON_MAX_LENGTH * 4;

export interface UiLayoutApplyClock {
  setTimeout(callback: () => void, delayMs: number): Timer;
  clearTimeout(timer: Timer): void;
}

export interface UiLayoutApplyTarget {
  readonly fingerprint: string;
  readonly tabId: string;
  readonly socketId: string;
}

export interface UiLayoutApplyPublication extends UiLayoutApplyTarget {
  readonly correlationId: string;
}

export interface UiLayoutApplyResolution {
  readonly outcome: UiApplyLayoutOutcome;
  readonly correlationId: string;
  readonly reason?: string;
}

export interface UiLayoutApplyOwnerOptions {
  readonly clock?: UiLayoutApplyClock;
  readonly timeoutMs?: number;
  readonly maxPending?: number;
  readonly maxTargetsPerFingerprint?: number;
  readonly maxTargetsTotal?: number;
  readonly createCorrelationId?: () => string;
}

interface PendingLayoutApply extends UiLayoutApplyPublication {
  readonly signal: AbortSignal;
  readonly abortListener: () => void;
  readonly resolve: (result: UiLayoutApplyResolution) => void;
  readonly reject: (error: Error) => void;
  timer: Timer | null;
}

const realClock: UiLayoutApplyClock = {
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (timer) => clearTimeout(timer),
};

export class UiLayoutApplyCanceledError extends Error {
  constructor() {
    super("layout apply cancelled");
    this.name = "UiLayoutApplyCanceledError";
  }
}

export class UiLayoutApplyCapacityError extends Error {
  constructor() {
    super("layout apply capacity exhausted");
    this.name = "UiLayoutApplyCapacityError";
  }
}

export class UiLayoutApplyOwner {
  private readonly targets = new Map<string, UiLayoutApplyTarget>();
  private readonly pending = new Map<string, PendingLayoutApply>();
  private readonly targetCountsByFingerprint = new Map<string, number>();
  private readonly clock: UiLayoutApplyClock;
  private readonly timeoutMs: number;
  private readonly maxPending: number;
  private readonly maxTargetsPerFingerprint: number;
  private readonly maxTargetsTotal: number;
  private readonly createCorrelationId: () => string;

  constructor(options: UiLayoutApplyOwnerOptions = {}) {
    this.clock = options.clock ?? realClock;
    this.timeoutMs = options.timeoutMs ?? UI_LAYOUT_APPLY_TIMEOUT_MS;
    this.maxPending = options.maxPending ?? UI_LAYOUT_APPLY_MAX_PENDING;
    this.maxTargetsPerFingerprint = options.maxTargetsPerFingerprint
      ?? UI_STATE_MAX_TABS_PER_FINGERPRINT;
    this.maxTargetsTotal = options.maxTargetsTotal
      ?? UI_STATE_MAX_TABS_TOTAL;
    this.createCorrelationId = options.createCorrelationId ?? randomUUID;
    requirePositiveSafeInteger(this.timeoutMs, "layout apply timeout");
    requirePositiveSafeInteger(this.maxPending, "layout apply capacity");
    requirePositiveSafeInteger(
      this.maxTargetsPerFingerprint,
      "layout target per-fingerprint capacity",
    );
    requirePositiveSafeInteger(this.maxTargetsTotal, "layout target capacity");
  }

  /** Register after the subscribed frame is sent. The disposer cannot remove a replacement. */
  registerTarget(target: UiLayoutApplyTarget): () => void {
    const key = targetKey(target.fingerprint, target.tabId);
    const previous = this.targets.get(key);
    if (!previous) {
      if (
        (this.targetCountsByFingerprint.get(target.fingerprint) ?? 0)
          >= this.maxTargetsPerFingerprint
        || this.targets.size >= this.maxTargetsTotal
      ) {
        throw new UiLayoutApplyCapacityError();
      }
      incrementCount(this.targetCountsByFingerprint, target.fingerprint);
    }
    this.targets.set(key, target);
    if (previous) this.settleTargetGoneForTarget(previous, "replaced");
    log.debug("ui-layout-apply", previous ? "target_replaced" : "target_registered", {
      caller_fp: target.fingerprint,
      tab_id: target.tabId,
      socket_id: target.socketId,
    });
    return () => {
      if (this.targets.get(key) !== target) return;
      this.targets.delete(key);
      decrementCount(this.targetCountsByFingerprint, target.fingerprint);
      this.settleTargetGoneForTarget(target, "closed");
      log.debug("ui-layout-apply", "target_unregistered", {
        caller_fp: target.fingerprint,
        tab_id: target.tabId,
        socket_id: target.socketId,
      });
    };
  }

  /** Reserve the selected device's exact live tab socket before publishing once. */
  requestApply(
    targetFingerprint: string,
    targetTabId: string,
    signal: AbortSignal,
    publish: (publication: UiLayoutApplyPublication) => void,
  ): Promise<UiLayoutApplyResolution> {
    const correlationId = this.allocateCorrelationId();
    if (signal.aborted) return Promise.reject(new UiLayoutApplyCanceledError());
    const target = this.targets.get(targetKey(
      targetFingerprint,
      targetTabId,
    ));
    if (!target) return Promise.resolve(targetGone(correlationId));
    if (this.pending.size >= this.maxPending) {
      return Promise.reject(new UiLayoutApplyCapacityError());
    }

    const { promise, resolve, reject } = Promise.withResolvers<UiLayoutApplyResolution>();
    let pending!: PendingLayoutApply;
    const abortListener = () => {
      if (!this.removePending(pending)) return;
      reject(new UiLayoutApplyCanceledError());
      log.debug("ui-layout-apply", "pending_cancelled", {
        correlation_id: correlationId,
        socket_id: target.socketId,
      });
    };
    pending = {
      ...target,
      correlationId,
      signal,
      abortListener,
      resolve,
      reject,
      timer: null,
    };
    this.pending.set(correlationId, pending);
    signal.addEventListener("abort", abortListener, { once: true });
    pending.timer = this.clock.setTimeout(() => {
      this.resolveTargetGone(pending, "timeout");
    }, this.timeoutMs);
    pending.timer.unref?.();
    log.debug("ui-layout-apply", "pending_registered", {
      correlation_id: correlationId,
      socket_id: target.socketId,
      timeout_ms: this.timeoutMs,
    });

    if (signal.aborted) abortListener();
    if (this.pending.get(correlationId) === pending) {
      try {
        publish(pending);
      } catch {
        this.resolveTargetGone(pending, "publication_failed");
      }
    }
    return promise;
  }

  /** Accept only browser-proven applied/rejected results on every stored fence. */
  acceptResult(source: UiLayoutApplyTarget, result: UiApplyLayoutResult): boolean {
    if (
      result.outcome !== UiApplyLayoutOutcome.APPLIED
      && result.outcome !== UiApplyLayoutOutcome.REJECTED
    ) return false;
    const pending = this.pending.get(result.correlationId);
    const currentTarget = this.targets.get(targetKey(
      source.fingerprint,
      source.tabId,
    ));
    if (
      !pending
      || pending.fingerprint !== source.fingerprint
      || pending.tabId !== source.tabId
      || pending.socketId !== source.socketId
      || currentTarget?.socketId !== source.socketId
    ) return false;
    if (!this.removePending(pending)) return false;
    const resolution = result.outcome === UiApplyLayoutOutcome.APPLIED
      ? { outcome: UiApplyLayoutOutcome.APPLIED, correlationId: pending.correlationId }
      : {
          outcome: UiApplyLayoutOutcome.REJECTED,
          correlationId: pending.correlationId,
          reason: sanitizeRejectedReason(result.reason),
        };
    pending.resolve(resolution);
    log.debug("ui-layout-apply", "pending_resolved", {
      correlation_id: pending.correlationId,
      socket_id: pending.socketId,
      outcome: result.outcome === UiApplyLayoutOutcome.APPLIED ? "applied" : "rejected",
    });
    return true;
  }

  dispose(): void {
    this.targets.clear();
    this.targetCountsByFingerprint.clear();
    for (const pending of [...this.pending.values()]) {
      this.resolveTargetGone(pending, "owner_disposed");
    }
  }

  /** Focused diagnostics/test seam; payloads and browser reasons are never retained here. */
  stats(): { targets: number; pending: number } {
    return { targets: this.targets.size, pending: this.pending.size };
  }

  private allocateCorrelationId(): string {
    for (let attempt = 0; attempt < 8; attempt += 1) {
      const correlationId = this.createCorrelationId();
      if (correlationId && !this.pending.has(correlationId)) return correlationId;
    }
    throw new Error("unable to allocate layout apply correlation id");
  }

  private settleTargetGoneForTarget(target: UiLayoutApplyTarget, cause: string): void {
    for (const pending of [...this.pending.values()]) {
      if (
        pending.fingerprint === target.fingerprint
        && pending.tabId === target.tabId
        && pending.socketId === target.socketId
      ) {
        this.resolveTargetGone(pending, cause);
      }
    }
  }

  private resolveTargetGone(pending: PendingLayoutApply, cause: string): void {
    if (!this.removePending(pending)) return;
    pending.resolve(targetGone(pending.correlationId));
    log.debug("ui-layout-apply", "pending_target_gone", {
      correlation_id: pending.correlationId,
      socket_id: pending.socketId,
      cause,
    });
  }

  private removePending(pending: PendingLayoutApply): boolean {
    if (this.pending.get(pending.correlationId) !== pending) return false;
    this.pending.delete(pending.correlationId);
    if (pending.timer !== null) this.clock.clearTimeout(pending.timer);
    pending.timer = null;
    pending.signal.removeEventListener("abort", pending.abortListener);
    return true;
  }
}

function targetKey(fingerprint: string, tabId: string): string {
  return JSON.stringify([fingerprint, tabId]);
}

function incrementCount(counts: Map<string, number>, key: string): void {
  counts.set(key, (counts.get(key) ?? 0) + 1);
}

function decrementCount(counts: Map<string, number>, key: string): void {
  const count = counts.get(key);
  if (count === undefined || count <= 1) counts.delete(key);
  else counts.set(key, count - 1);
}

function requirePositiveSafeInteger(value: number, field: string): void {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new RangeError(`${field} must be a positive safe integer`);
  }
}

function targetGone(correlationId: string): UiLayoutApplyResolution {
  return {
    outcome: UiApplyLayoutOutcome.TARGET_GONE,
    correlationId,
    reason: UI_LAYOUT_TARGET_GONE_REASON,
  };
}

function sanitizeRejectedReason(reason: string | undefined): string {
  let clean = "";
  let outputCodePoints = 0;
  let inspectedCodePoints = 0;
  let pendingSpace = false;
  for (const character of reason ?? "") {
    if (
      inspectedCodePoints++ >= UI_LAYOUT_REJECTED_REASON_MAX_INSPECTED_CODE_POINTS
      || outputCodePoints >= UI_LAYOUT_REJECTED_REASON_MAX_LENGTH
    ) break;
    if (/[\p{Cc}\p{Cf}\s]/u.test(character)) {
      if (outputCodePoints > 0) pendingSpace = true;
      continue;
    }
    if (pendingSpace) {
      if (outputCodePoints >= UI_LAYOUT_REJECTED_REASON_MAX_LENGTH - 1) break;
      clean += " ";
      outputCodePoints++;
      pendingSpace = false;
    }
    clean += character;
    outputCodePoints++;
  }
  return clean || UI_LAYOUT_REJECTED_REASON_FALLBACK;
}
