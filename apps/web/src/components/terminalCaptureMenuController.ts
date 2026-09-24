// Consent-gated control seam between the terminal context menu and the incident
// recorder: owns the lease UI subscription, the one-request-in-flight guard, the
// frozen-evidence token lifecycle and the once-per-capture-ID result announcement.
// TerminalContextMenu and TerminalCaptureConsentDialog are its only consumers.
// Depends on lib/terminalIncidentCapture.ts (recorder seam) and
// lib/terminalCaptureDownload.ts (result toasts + authenticated download).

import { createEffect, createSignal, onCleanup, type Accessor } from "solid-js";
import {
  TERMINAL_CAPTURE_LIMITS,
  type TerminalCaptureActionName,
  type TerminalCaptureResult,
} from "@roost/protocol/terminal-capture";
import { diag } from "@roost/observability/diag";
import {
  announceTerminalCaptureException,
  announceTerminalCaptureResult,
} from "../lib/terminalCaptureDownload.ts";
import {
  captureTerminalIncidentFrozen,
  discardTerminalCaptureEvidence,
  downloadLocalTerminalEvidence,
  freezeTerminalCaptureEvidence,
  startTerminalCapture,
  stopTerminalCapture,
  subscribeTerminalCaptureUiState,
  terminalCaptureUiState,
  type TerminalCaptureUiState,
} from "../lib/terminalIncidentCapture.ts";

export type TerminalCaptureConsentKind = "start" | "capture";

export interface TerminalCaptureMenuController {
  readonly captureState: Accessor<TerminalCaptureUiState>;
  readonly consentKind: Accessor<TerminalCaptureConsentKind | null>;
  readonly startDisabled: Accessor<boolean>;
  readonly captureDisabled: Accessor<boolean>;
  readonly stopDisabled: Accessor<boolean>;
  requestStartDebugging(): void;
  requestCapture(): void;
  requestStopDebugging(): void;
  confirmConsent(): void;
  cancelConsent(): void;
}

interface PendingConsent {
  readonly kind: TerminalCaptureConsentKind;
  readonly token: string;
}

export function createTerminalCaptureMenuController(
  sessionId: Accessor<string>,
): TerminalCaptureMenuController {
  const [captureState, setCaptureState] = createSignal<TerminalCaptureUiState>(
    terminalCaptureUiState(sessionId()),
  );
  const [pending, setPending] = createSignal<PendingConsent | null>(null);
  const [inFlight, setInFlight] = createSignal(false);

  // Capture IDs this controller already reported, so the lease-state echo of a
  // capture the operator just requested is not announced twice.
  const reportedCaptureIds = new Set<string>();

  const remember = (captureId: string): void => {
    if (reportedCaptureIds.size >= TERMINAL_CAPTURE_LIMITS.completedCaptureIds) {
      reportedCaptureIds.clear();
    }
    reportedCaptureIds.add(captureId);
  };

  createEffect(() => {
    const sid = sessionId();
    setCaptureState(terminalCaptureUiState(sid));
    onCleanup(subscribeTerminalCaptureUiState(sid, (next: TerminalCaptureUiState) => {
      setCaptureState(next);
      // A recorder-triggered capture (history/viewport/worker detector) never
      // passes through this menu, so the operator learns about it here.
      const result = next.lastResult;
      if (!result || reportedCaptureIds.has(result.capture_id)) return;
      remember(result.capture_id);
      announceTerminalCaptureResult(result, { trigger: "automatic" });
    }));
  });

  const settle = async (
    action: TerminalCaptureActionName,
    work: () => Promise<TerminalCaptureResult>,
    token: string | null,
  ): Promise<TerminalCaptureResult | null> => {
    if (inFlight()) return null;
    setInFlight(true);
    try {
      const result = await work();
      // The recorder may publish the lease state carrying this result before its
      // promise resolves, so one capture ID is announced exactly once.
      if (!reportedCaptureIds.has(result.capture_id)) {
        remember(result.capture_id);
        announceTerminalCaptureResult(result, {
          trigger: "manual",
          onDownloadLocalEvidence: token ? () => downloadLocalTerminalEvidence(token) : null,
        });
      }
      return result;
    } catch (error) {
      diag("diag.terminal_capture_ui_failed", {
        sid: sessionId(),
        action,
        kind: error instanceof Error ? error.name : "unknown",
      });
      announceTerminalCaptureException(action, sessionId());
      return null;
    } finally {
      setInFlight(false);
    }
  };

  // One outstanding request per session. The menu items are disabled while one
  // is in flight, so this guard only catches a race — and it refuses BEFORE
  // freezing, because a frozen payload nobody sends is retained evidence.
  const requestStartDebugging = (): void => {
    if (inFlight()) return;
    const sid = sessionId();
    // Freeze first: the confirmation dialog moves focus and can change reader
    // holds, so the on-screen evidence must be owned before it opens.
    const token = freezeTerminalCaptureEvidence(sid, "manual");
    diag("diag.terminal_capture_consent_open", { sid, kind: "start" });
    setPending({ kind: "start", token });
  };

  const requestCapture = (): void => {
    if (inFlight()) return;
    const sid = sessionId();
    const token = freezeTerminalCaptureEvidence(sid, "manual");
    if (captureState().phase === "recording") {
      void settle("capture", () => captureTerminalIncidentFrozen(token), token);
      return;
    }
    diag("diag.terminal_capture_consent_open", { sid, kind: "capture" });
    setPending({ kind: "capture", token });
  };

  const requestStopDebugging = (): void => {
    if (inFlight()) return;
    const request = pending();
    if (request) {
      setPending(null);
      discardTerminalCaptureEvidence(request.token);
    }
    void settle("stop", () => stopTerminalCapture(sessionId()), null);
  };

  const confirmConsent = (): void => {
    const request = pending();
    if (!request) return;
    setPending(null);
    void (async () => {
      if (request.kind === "capture") {
        await settle("capture", () => captureTerminalIncidentFrozen(request.token), request.token);
        return;
      }
      const started = await settle("start", () => startTerminalCapture(sessionId()), request.token);
      // Arming writes no bundle: the pre-arm freeze exists only so the dialog
      // cannot change reader holds before consent. A lease that failed to arm
      // keeps the frozen payload for retry.
      if (started?.status === "recording") discardTerminalCaptureEvidence(request.token);
    })();
  };

  const cancelConsent = (): void => {
    const request = pending();
    if (!request) return;
    setPending(null);
    discardTerminalCaptureEvidence(request.token);
    diag("diag.terminal_capture_consent_cancelled", { sid: sessionId(), kind: request.kind });
  };

  const phase = () => captureState().phase;

  return {
    captureState,
    consentKind: () => pending()?.kind ?? null,
    startDisabled: () => inFlight() || phase() === "recording" || phase() === "arming",
    captureDisabled: () => inFlight() || phase() === "arming",
    stopDisabled: () => inFlight() || phase() === "idle",
    requestStartDebugging,
    requestCapture,
    requestStopDebugging,
    confirmConsent,
    cancelConsent,
  };
}
