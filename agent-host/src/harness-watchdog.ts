// Detects the harness states pi cannot recover from in-process and reports them once.
// pi poisons its Session when a commit fails after storage admission ("reopen it") and
// offers no event for it, so health is probed with an empty commit: it runs pi's own
// usability check and writes nothing. A close nobody asked for is fatal too.
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import type { Harness } from "@earendil-works/pi-durable";

export const HEALTH_PROBE_INTERVAL_MS = 15_000;

export interface HarnessWatchdog {
  /** Probe now; reports through `onFatal` when the harness is no longer usable. */
  check(reason: string): Promise<void>;
  /** The coming close is ours; it must not be reported. */
  markShuttingDown(): void;
  stop(): void;
}

export function watchHarness(
  harness: Harness,
  onFatal: (reason: string, cause: unknown) => void,
  intervalMs = HEALTH_PROBE_INTERVAL_MS,
): HarnessWatchdog {
  let shuttingDown = false;
  let reported = false;
  const report = (reason: string, cause: unknown): void => {
    if (shuttingDown || reported) return;
    reported = true;
    onFatal(reason, cause);
  };
  const unsubscribeClose = harness.subscribeClose(() => report("harness closed unexpectedly", undefined));
  const check = async (reason: string): Promise<void> => {
    if (shuttingDown || reported) return;
    try {
      await harness.commit(() => undefined, BACKGROUND_CONTEXT);
    } catch (error) {
      report(`harness unusable after ${reason}`, error);
    }
  };
  const timer = setInterval(() => void check("periodic probe"), intervalMs);
  timer.unref();
  return {
    check,
    markShuttingDown: () => { shuttingDown = true; },
    stop: () => { clearInterval(timer); unsubscribeClose(); },
  };
}
