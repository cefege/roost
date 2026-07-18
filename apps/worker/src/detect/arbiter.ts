// Pure agent-status arbitration (ports herdr src/pane/agent_detection.rs, coord
// edition). Coord is the detection authority: it runs the manifest over one
// headless grid per session (claude-status-hub) and arbitrates the verdict into
// a stable status. Pure → exhaustively unit-testable (arbiter.test.ts).
//
// "working" comes from the MANIFEST (claude's OSC braille-spinner title), NOT
// raw byte activity — so a plain shell echoing output is never mistaken for a
// running agent. Byte-activity timing is used ONLY for the working→idle HOLD:
// don't commit idle the instant the spinner blinks off mid-turn; wait until the
// output stream is genuinely quiet.

export type ArbStatus = "running" | "needs-input" | "idle" | "done";

export interface ArbiterInputs {
  /** Last committed status for this session (undefined = nothing emitted yet). */
  prev: ArbStatus | undefined;
  /** Manifest verdict mapped to status: running/needs-input/idle, or null = no
   *  opinion (not a recognized agent screen). The screen never yields "done". */
  screenStatus: Exclude<ArbStatus, "done"> | null;
  /** Manifest matched a live blocker (permission / selection prompt). */
  screenBlocker: boolean;
  /** PTY bytes flowed within the working-grace window (burst not yet settled). */
  recentBytes: boolean;
}

export interface ArbiterResult {
  /** Status to commit, or undefined = no opinion (hold / don't publish). */
  next: ArbStatus | undefined;
  /** We're holding running over a transient idle because bytes are still recent
   *  — re-check once the stream goes quiet so idle eventually commits. */
  reevalForIdle: boolean;
}

export function resolveAgentStatus(i: ArbiterInputs): ArbiterResult {
  const blocker = i.screenBlocker || i.screenStatus === "needs-input";
  if (blocker) return { next: "needs-input", reevalForIdle: false };
  if (i.screenStatus === "running") return { next: "running", reevalForIdle: false };
  if (i.screenStatus === "idle") {
    // working→idle hold: the spinner just blinked off but the turn may not be
    // done (a gap between tool calls). Hold running until the stream is quiet.
    if (i.prev === "running" && i.recentBytes) return { next: "running", reevalForIdle: true };
    return { next: "idle", reevalForIdle: false };
  }
  return { next: i.prev, reevalForIdle: false }; // null verdict → hold (no publish on undefined)
}
