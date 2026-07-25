// ─── Assistant turn-ending notice ─────────────────────────────────────────
// Port of @oh-my-pi/pi-coding-agent@17.1.3
//   src/modes/utils/transcript-render-helpers.ts::resolveAssistantErrorPresentation
//   src/session/messages.ts::{isSilentAbort,isUserInterruptAbort,resolveAbortLabel}
// omp's live-only `retryAttempt` branch ("Aborted after N retry attempts") is
// deliberately NOT ported: Roost replays persisted history, where retryAttempt
// is always 0, so that branch is unreachable here.
//
// Lives in shared, not in the worker, because BOTH row projections need it and
// the browser oracle projects rows with the same code — a second copy would
// drift, and a drifted oracle proves nothing.

type Rec = Record<string, unknown>;
const asStr = (x: unknown): string | undefined => (typeof x === "string" ? x : undefined);
const asRec = (x: unknown): Rec | undefined => (typeof x === "object" && x !== null ? (x as Rec) : undefined);
const asNum = (x: unknown): number | undefined => (typeof x === "number" && Number.isFinite(x) ? x : undefined);

const SILENT_ABORT_MARKER = "__omp.silent_abort__";
const USER_INTERRUPT_LABEL = "Interrupted by user";
const GENERIC_ABORT_SENTINEL = "Request was aborted";
// @oh-my-pi/pi-ai src/error/flags.ts — AIError.is(id, f) === ((id ?? 0) & f) !== 0.
const FLAG_SILENT_ABORT = 0x0200_0000;
const FLAG_USER_INTERRUPT = 0x0400_0000;
const FLAG_ABORT = 0x0800_0000;

const hasFlag = (errorId: unknown, flag: number): boolean => ((asNum(errorId) ?? 0) & flag) !== 0;

/** The turn-ending line omp paints under an assistant message, or null when it
 *  paints none (silent aborts and Esc interrupts are quiet by design). */
export function resolveAssistantNotice(m: Rec): { level: "error" | "note"; text: string } | null {
  const errorMessage = asStr(m.errorMessage);
  const silentAbort = hasFlag(m.errorId, FLAG_SILENT_ABORT) || errorMessage === SILENT_ABORT_MARKER;
  const userInterrupt = hasFlag(m.errorId, FLAG_USER_INTERRUPT) || errorMessage === USER_INTERRUPT_LABEL;
  const renderAbortReason = !silentAbort && !userInterrupt;

  const recovery = asRec(m.retryRecovery);
  if (recovery?.status === "recovered") {
    const note = (asStr(recovery.note) ?? "").replace(/\s+/g, " ").trim();
    return { level: "note", text: note || "retried" };
  }
  if (m.stopReason === "aborted") {
    if (!renderAbortReason) return null;
    const generic = hasFlag(m.errorId, FLAG_ABORT)
      || errorMessage === GENERIC_ABORT_SENTINEL || silentAbort;
    if (generic || !errorMessage) return { level: "error", text: "Operation aborted" };
    return { level: "error", text: errorMessage };
  }
  if (m.stopReason === "error") return { level: "error", text: errorMessage || "Error" };
  if (errorMessage && renderAbortReason) return { level: "error", text: errorMessage };
  return null;
}
