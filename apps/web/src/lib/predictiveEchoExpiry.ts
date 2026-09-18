// Bounded expiry for predictive local echo. A prediction is settled by an
// authoritative frame, but a non-echoing prompt (sudo, ssh password) produces
// no later frame at all, so a guess would stay painted forever. This owns the
// single deferred pass that abandons one: predictiveEcho.ts re-arms it whenever
// the prediction set changes and calls check() when it fires.

const PREDICTION_EXPIRE_FLOOR_MS = 1000;

export class PredictionExpiryTimer {
  private cancelPending: (() => void) | null = null;
  private readonly now: () => number;
  private readonly schedule: (callback: () => void, delayMs: number) => () => void;
  private readonly oldestBornMs: () => number | null;
  private readonly srtt: () => number;
  private readonly onExpire: () => void;

  constructor(opts: {
    now: () => number;
    schedule: (callback: () => void, delayMs: number) => () => void;
    /** Birth time of the oldest live prediction, or null when there are none. */
    oldestBornMs: () => number | null;
    srtt: () => number;
    onExpire: () => void;
  }) {
    this.now = opts.now;
    this.schedule = opts.schedule;
    this.oldestBornMs = opts.oldestBornMs;
    this.srtt = opts.srtt;
    this.onExpire = opts.onExpire;
  }

  /** Re-arm for the oldest live prediction. No predictions → stay disarmed. */
  arm(): void {
    this.cancel();
    const oldest = this.oldestBornMs();
    if (oldest === null) return;
    const delayMs = Math.max(1, oldest + this.windowMs() - this.now());
    this.cancelPending = this.schedule(() => {
      this.cancelPending = null;
      this.check();
    }, delayMs);
  }

  /** Expire when the oldest prediction outlived the window, else re-arm. */
  check(): void {
    this.cancel();
    const oldest = this.oldestBornMs();
    if (oldest !== null && this.now() - oldest >= this.windowMs()) {
      this.onExpire();
      return;
    }
    this.arm();
  }

  cancel(): void {
    this.cancelPending?.();
    this.cancelPending = null;
  }

  /** A measured slow link widens the window, so a legitimate prediction on a
   *  genuinely laggy connection is never expired out from under its echo. */
  private windowMs(): number {
    return Math.max(PREDICTION_EXPIRE_FLOOR_MS, this.srtt() * 4);
  }
}
