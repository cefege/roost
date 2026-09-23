// Finite authority lease for one admitted direct attachment port.
// Grant expiry gates new hello admission; this timer instead bounds an already
// authenticated descriptor by a hard lifetime and valid-activity idle deadline.

import {
  ATTACHMENT_TRANSFER_ACTIVE_MAX_MS,
  ATTACHMENT_TRANSFER_IDLE_MS,
} from "@roost/shared/attachment-transfer";

export interface AttachmentTransferLeaseOptions {
  readonly onExpired: () => void;
  readonly now?: () => number;
  readonly scheduleTimeout?: (callback: () => void, delayMs: number) => NodeJS.Timeout;
  readonly clearTimeout?: (timer: NodeJS.Timeout) => void;
}

export class AttachmentTransferLease {
  private readonly now: () => number;
  private readonly scheduleTimeout: (callback: () => void, delayMs: number) => NodeJS.Timeout;
  private readonly clearScheduledTimeout: (timer: NodeJS.Timeout) => void;
  private hardDeadlineMs = 0;
  private idleDeadlineMs = 0;
  private timer: NodeJS.Timeout | undefined;
  private expired = false;

  constructor(private readonly options: AttachmentTransferLeaseOptions) {
    this.now = options.now ?? (() => performance.now());
    this.scheduleTimeout = options.scheduleTimeout ?? ((callback, delayMs) => setTimeout(callback, delayMs));
    this.clearScheduledTimeout = options.clearTimeout ?? ((timer) => clearTimeout(timer));
  }

  start(): void {
    const now = this.now();
    this.hardDeadlineMs = now + ATTACHMENT_TRANSFER_ACTIVE_MAX_MS;
    this.idleDeadlineMs = now + ATTACHMENT_TRANSFER_IDLE_MS;
    this.arm();
  }

  allowsActivity(): boolean {
    if (!this.expired && this.now() < Math.min(this.hardDeadlineMs, this.idleDeadlineMs)) return true;
    this.expire();
    return false;
  }

  noteValidActivity(): boolean {
    if (!this.allowsActivity()) return false;
    this.idleDeadlineMs = Math.min(this.hardDeadlineMs, this.now() + ATTACHMENT_TRANSFER_IDLE_MS);
    this.arm();
    return true;
  }

  dispose(): void {
    if (this.timer) this.clearScheduledTimeout(this.timer);
    this.timer = undefined;
  }

  private arm(): void {
    if (this.timer) this.clearScheduledTimeout(this.timer);
    const delayMs = Math.max(0, Math.min(this.hardDeadlineMs, this.idleDeadlineMs) - this.now());
    this.timer = this.scheduleTimeout(() => {
      this.timer = undefined;
      if (!this.allowsActivity()) return;
      this.arm();
    }, delayMs);
    this.timer.unref?.();
  }

  private expire(): void {
    if (this.expired) return;
    this.expired = true;
    this.dispose();
    this.options.onExpired();
  }
}
