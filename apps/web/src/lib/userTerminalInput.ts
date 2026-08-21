import { diag, signal } from "@roost/shared/diag";
import { sendTerminalInput, type InputAdmission } from "../ws/sync-outbound.ts";

type UserTerminalInputCallback = () => void;

interface Registration {
  callback: UserTerminalInputCallback;
}

const registrations = new Map<string, Registration>();

/** Register the pane transition that must accompany admitted local PTY input. */
export function registerUserTerminalInput(
  sessionId: string,
  callback: UserTerminalInputCallback,
): () => void {
  const registration = { callback };
  registrations.set(sessionId, registration);
  return () => {
    if (registrations.get(sessionId) === registration) registrations.delete(sessionId);
  };
}

/**
 * Admit bytes to the bounded terminal input lane, then synchronously notify the
 * currently mounted pane. Rejected input is passive and never changes its view.
 */
export function sendUserTerminalInput(
  sessionId: string,
  bytes: Uint8Array,
  viewId?: string,
): InputAdmission {
  const admission = sendTerminalInput(sessionId, bytes, viewId);
  if (!admission.accepted) return admission;

  const registration = registrations.get(sessionId);
  if (!registration) return admission;

  try {
    registration.callback();
  } catch (error) {
    let detail = "unprintable callback error";
    try {
      detail = error instanceof Error ? error.message : String(error);
    } catch {
      // Keep failure reporting best-effort so admission is never changed.
    }
    try {
      diag("input.user_callback_failed", { sid: sessionId, detail });
    } catch {
      // A diagnostic sink must not turn admitted input into a caller failure.
    }
    try {
      signal("diag.corruption_signal", {
        kind: "user_terminal_input_callback_failed",
        sid: sessionId,
        detail,
        cooldownKey: sessionId,
      });
    } catch {
      // Preserve the original admission even if the signal sink is faulty.
    }
    try {
      console.warn("[terminal-input] user callback failed", error);
    } catch {
      // Console implementations are also outside the admission contract.
    }
  }
  return admission;
}

/** Deterministic registry reset for focused unit tests. */
export function _resetUserTerminalInputForTest(): void {
  registrations.clear();
}
