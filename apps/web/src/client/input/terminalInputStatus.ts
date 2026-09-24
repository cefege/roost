// Maps a settled terminal input outcome to the composer's status line and to
// whether the submitted text belongs back in the composer. Called by
// components/TerminalComposeButton.tsx; depends only on the input-lane
// vocabulary in ws/terminal-input-lanes.ts, so the mapping is testable without
// a DOM and the composer stays small.

import type { InputOutcome } from "../carriers/terminal-input-lanes.ts";

export interface ComposerSubmissionStatus {
  /** Status line for the composer, or null when nothing needs saying. */
  message: string | null;
  /** Whether the submitted text belongs back in an empty composer. */
  restoreDraft: boolean;
}

const MAX_REASON_CHARS = 160;

export function describeInputOutcome(outcome: InputOutcome): ComposerSubmissionStatus {
  if (outcome.status === "accepted") return { message: null, restoreDraft: false };
  const reason = normalizeReason(outcome.reason);
  if (outcome.status === "rejected") {
    return { message: `Not sent — ${reason}`, restoreDraft: true };
  }
  if (outcome.writtenBytes > 0) {
    return {
      message: `Partially sent (${outcome.writtenBytes} bytes) — ${reason}. It was not retried.`,
      restoreDraft: true,
    };
  }
  return {
    message: `Delivery unconfirmed — ${reason}. Nothing was resent; check the terminal before sending again.`,
    restoreDraft: true,
  };
}

function normalizeReason(reason: string): string {
  const flattened = reason.replace(/\s+/g, " ").trim().slice(0, MAX_REASON_CHARS);
  return flattened === "" ? "no reason reported" : flattened;
}
