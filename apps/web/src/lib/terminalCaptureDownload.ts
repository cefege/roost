// Authenticated retrieval and operator notification for saved terminal incident
// bundles. TerminalCaptureConsentDialog's menu controller hands it every
// TerminalCaptureResult; it resolves the owning worker's file href and reuses the
// chunked authenticated download loop, so a sensitive bundle never travels over an
// unauthenticated route and never lands among ordinary attachments.
// Depends on nativePath.ts (href shape), downloadWorkerFile.ts (filesReadChunk) and toastStore.ts.

import {
  TERMINAL_CAPTURE_LIMITS,
  terminalCaptureFileName,
  type TerminalCaptureActionName,
  type TerminalCaptureResult,
} from "@roost/protocol/terminal-capture";
import { diag } from "@roost/observability/diag";
import { addToast } from "../store/toastStore.ts";
import { downloadWorkerFileByHref } from "./downloadWorkerFile.ts";
import { workerFileHref } from "./nativePath.ts";

export type TerminalCaptureTrigger = "manual" | "automatic";

export interface TerminalCaptureAnnounceOptions {
  readonly trigger: TerminalCaptureTrigger;
  /** Offered only while the browser still holds its frozen payload because the
   *  bundle never reached a worker; the recorder owns the local JSON export. */
  readonly onDownloadLocalEvidence?: (() => void) | null;
}

/** `/file/<workerFp>/<encoded path>` for a saved bundle, or null when the
 *  result names no worker-local file. */
function terminalCaptureBundleHref(
  workerFp: string | null,
  path: string | null,
): string | null {
  if (!workerFp || !path) return null;
  return workerFileHref(workerFp, path);
}

/** Pull one saved bundle through the authenticated chunked file read. Returns
 *  false when the result carries no downloadable worker file. */
async function downloadTerminalCaptureBundle(
  workerFp: string | null,
  path: string | null,
  captureId: string,
): Promise<boolean> {
  const href = terminalCaptureBundleHref(workerFp, path);
  if (!href) return false;
  diag("diag.terminal_capture_download", { capture_id: captureId, worker_fp: workerFp });
  await downloadWorkerFileByHref(href);
  return true;
}

/** One toast per capture outcome. A manual capture starts its download
 *  immediately; an automatic capture offers the same download as an action so a
 *  worker- or browser-triggered incident is not lost when nobody was looking. */
export function announceTerminalCaptureResult(
  result: TerminalCaptureResult,
  options: TerminalCaptureAnnounceOptions,
): void {
  switch (result.action) {
    case "start":
      announceLeaseTransition(result, "Terminal debugging recording");
      break;
    case "stop":
      announceLeaseTransition(result, "Terminal debugging stopped · saved captures kept");
      break;
    case "capture":
      announceCapture(result, options);
      break;
  }
  announceRecentWorkerCapture(result);
}

/** A thrown transport fault carries no result to project, so it reports the same
 *  fixed shape: an exception must not tell the operator less than an RPC did. */
export function announceTerminalCaptureException(
  action: TerminalCaptureActionName,
  sessionId: string,
): void {
  addToast(`${FAILURE_HEADLINE[action]} · internal`, "err", {
    details: [`session ${sessionId}`, "error: internal", "browser evidence: retained"].join("\n"),
  });
}

// Capture IDs already surfaced, so a lease renewal that keeps reporting the same
// worker-local incident cannot raise a toast per renewal.
const announcedWorkerCaptures = new Set<string>();

const FAILURE_HEADLINE: Record<TerminalCaptureActionName, string> = {
  start: "Terminal debugging could not start",
  capture: "Terminal diagnostic failed",
  stop: "Terminal debugging could not stop",
};

function announceLeaseTransition(result: TerminalCaptureResult, okMessage: string): void {
  const settled = result.action === "start" ? "recording" : "stopped";
  if (result.status === settled) {
    addToast(okMessage, "ok");
    return;
  }
  addToast(`${FAILURE_HEADLINE[result.action]} · ${result.error ?? result.status}`, "err", {
    details: captureAvailability(result, null),
  });
}

function announceCapture(
  result: TerminalCaptureResult,
  options: TerminalCaptureAnnounceOptions,
): void {
  const localEvidence = options.onDownloadLocalEvidence ?? null;
  const href = terminalCaptureBundleHref(result.worker_fp, result.path);
  const download = href === null
    ? null
    : () => {
      void downloadTerminalCaptureBundle(result.worker_fp, result.path, result.capture_id);
    };
  // A manual capture IS a download request; an automatic one only offers it.
  if (download && options.trigger === "manual") download();
  const offer = download && options.trigger === "automatic"
    ? { label: "Download", onClick: download }
    : !download && localEvidence
      ? { label: "Download local evidence", onClick: localEvidence }
      : undefined;

  if (result.status === "captured") {
    addToast(
      download && options.trigger === "manual"
        ? `Terminal diagnostic captured · downloading ${terminalCaptureFileName(result.capture_id)}`
        : "Terminal diagnostic captured",
      "ok",
      {
        // A saved bundle needs no inventory; anything else must say what exists.
        details: download ? undefined : captureAvailability(result, localEvidence),
        action: offer,
      },
    );
    return;
  }

  // Partial and error both keep the frozen browser payload; the operator is told
  // which layers exist rather than being handed an empty success.
  const partial = result.status === "partial";
  addToast(
    partial
      ? "Terminal diagnostic captured (partial)"
      : `Terminal diagnostic failed · ${result.error ?? result.status}`,
    partial ? "warn" : "err",
    { details: captureAvailability(result, localEvidence), action: offer },
  );
}

function announceRecentWorkerCapture(result: TerminalCaptureResult): void {
  const recent = result.recent_worker_capture;
  if (!recent) return;
  const href = terminalCaptureBundleHref(result.worker_fp, recent.path);
  if (!href || announcedWorkerCaptures.has(recent.capture_id)) return;
  if (announcedWorkerCaptures.size >= TERMINAL_CAPTURE_LIMITS.completedCaptureIds) {
    announcedWorkerCaptures.clear();
  }
  announcedWorkerCaptures.add(recent.capture_id);
  addToast("Worker detected a terminal incident", "ok", {
    details: `saved on the worker · ${recent.status} · ${recent.byte_length} bytes`,
    action: {
      label: "Download",
      onClick: () => {
        void downloadTerminalCaptureBundle(result.worker_fp, recent.path, recent.capture_id);
      },
    },
  });
}

/** Content-free availability report: fixed codes, bounds and which layer's
 *  artifact exists. Never a validator message, which could quote terminal text. */
function captureAvailability(
  result: TerminalCaptureResult,
  localEvidence: (() => void) | null,
): string {
  const rows = [
    `capture ${result.capture_id}`,
    `worker bundle: ${result.path ? `saved (${result.byte_length ?? 0} bytes)` : "unavailable"}`,
  ];
  if (localEvidence) rows.push("browser evidence: held locally for retry");
  if (result.error) rows.push(`error: ${result.error}`);
  return rows.join("\n");
}

/** Tests assert one toast per distinct worker-local incident across renewals. */
export function _resetAnnouncedWorkerCaptures(): void {
  announcedWorkerCaptures.clear();
}
