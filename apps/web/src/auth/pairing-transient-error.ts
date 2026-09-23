// Transient-failure classification shared by the requester ceremony
// (components/onboarding-pairing-ceremony.ts) and the approver lifecycle
// (auth/pair-approval-lifecycle.ts). A transient failure retries with backoff
// and never discards a tab-scoped pairing capability or a displayed code.

import { Code, ConnectError } from "@connectrpc/connect";

const TRANSIENT_PAIRING_CODES: ReadonlySet<Code> = new Set([
  Code.Unknown,
  Code.Unavailable,
  Code.DeadlineExceeded,
  Code.Aborted,
  Code.ResourceExhausted,
]);

/** Non-Connect failures are transport-level and therefore transient too. */
export function isTransientPairingError(error: unknown): boolean {
  return !(error instanceof ConnectError) || TRANSIENT_PAIRING_CODES.has(error.code);
}
