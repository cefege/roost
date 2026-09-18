// ECHO_INPUT mode for the portable PTY fixture: once armed, every stdin byte is
// written straight back after a fixed per-chunk delay and nothing is parsed as a
// command again. pty-fixture.ts arms this from a parsed command and hands it the
// bytes; predictive-echo specs need a PTY whose echo latency they control.

import { boundedInteger } from "./pty-fixture-commands.ts";

const MAX_ECHO_DELAY_MS = 5_000;

export class EchoInputMode {
  private delayMs: number | null = null;
  private lane: Promise<void> = Promise.resolve();

  constructor(private readonly write: (output: string | Uint8Array) => Promise<void>) {}

  get armed(): boolean {
    return this.delayMs !== null;
  }

  /** Arm when `raw` is an ECHO_INPUT command, else return false and change
   *  nothing. `after` orders the first echoed byte behind whatever the fixture
   *  has already queued, which is what makes the ARMED marker observable. */
  armFromCommand(raw: unknown, after: Promise<void>): boolean {
    if (!raw || typeof raw !== "object" || !("op" in raw) || raw.op !== "ECHO_INPUT") return false;
    const delayMs = "delayMs" in raw ? raw.delayMs : undefined;
    this.delayMs = delayMs === undefined
      ? 0
      : boundedInteger(delayMs, "delayMs", 0, MAX_ECHO_DELAY_MS);
    this.lane = after;
    return true;
  }

  writeArmedMarker(): Promise<void> {
    return this.write("ECHO_INPUT_ARMED\r\n");
  }

  /** Queue one chunk, echoed `delayMs` after IT arrived rather than after the
   *  previous echo: a cumulative delay would drain the in-flight burst these
   *  specs exist to create. The lane only preserves order. */
  push(bytes: Buffer | Uint8Array): void {
    const owned = Buffer.from(bytes);
    const deadline = Date.now() + (this.delayMs ?? 0);
    this.lane = this.lane
      .then(async () => {
        const remaining = deadline - Date.now();
        if (remaining > 0) await Bun.sleep(remaining);
        await this.write(owned);
      })
      .catch(() => {});
  }
}
