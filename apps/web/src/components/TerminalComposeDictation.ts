// Owns the composer's dictation draft: the typed prefix a recording replaces
// its tail on, the last settled phrase an abandoned recording keeps, and where
// the unsettled hypothesis starts inside the painted draft.
// Called by TerminalComposeButton (engine callbacks from MobileVoiceInput);
// writes through the composer's programmatic-draft writer and draft persistence.

import { createSignal } from "solid-js";

interface TerminalComposeDictationOptions {
  /** The composer's current draft text. */
  draft: () => string;
  /** The composer's programmatic draft writer (writeSpeechDraft). */
  write: (next: string) => void;
  /** Persist a draft immediately, outside the reactive save effect. */
  persist: (text: string) => void;
}

/** Append dictated text to the typed prefix with exactly one separating space
 *  and never a leading one. */
const glued = (base: string, text: string) =>
  (base.length === 0 || base.endsWith(" ") ? base : `${base} `) + text;

/**
 * The draft a dictation update paints, plus the index where its unsettled
 * hypothesis starts (null when nothing is unsettled). The index is derived from
 * the hypothesis LENGTH rather than by matching a prefix, so a settled phrase
 * that is not a literal prefix of the painted text can never mis-split it.
 */
export function paintDictation(
  base: string,
  settled: string,
  hypothesis: string,
): { text: string; provisionalFrom: number | null } {
  const spoken = settled.length > 0 && hypothesis.length > 0
    ? `${settled} ${hypothesis}`
    : settled + hypothesis;
  const text = spoken.length === 0 ? base : glued(base, spoken);
  return {
    text,
    provisionalFrom: hypothesis.length === 0 ? null : text.length - hypothesis.length,
  };
}

export function createTerminalComposeDictation(options: TerminalComposeDictationOptions) {
  // dictationBase is the typed prefix captured when the mic opens; lastSettled
  // snapshots settled phrases so an abandoned dictation keeps real words while
  // dropping the hypothesis.
  let dictationBase: string | null = null;
  let lastSettled: string | null = null;
  const [provisionalFrom, setProvisionalFrom] = createSignal<number | null>(null);

  const show = (update: { settled: string; hypothesis: string } | null) => {
    if (update === null) {
      // Ended WITHOUT a commit (failure/empty/deactivation/unmount): keep base +
      // settled words and drop the hypothesis. Reverting to base alone silently
      // deleted dictated words; keeping the hypothesis baked a guess the engine
      // never settled into the draft.
      if (dictationBase === null) return;
      const kept = lastSettled ? glued(dictationBase, lastSettled) : dictationBase;
      dictationBase = null;
      lastSettled = null;
      setProvisionalFrom(null);
      options.write(kept);
      options.persist(kept);
      return;
    }
    if (dictationBase === null) {
      dictationBase = options.draft();
      lastSettled = null;
    }
    lastSettled = update.settled;
    const painted = paintDictation(dictationBase, update.settled, update.hypothesis);
    setProvisionalFrom(painted.provisionalFrom);
    options.write(painted.text);
  };

  return {
    /** Engine update while dictating; null ends it without a commit. */
    show,
    /** Final transcript on stop: the only path that keeps the hypothesis. */
    commit: (text: string) => {
      const base = dictationBase ?? options.draft();
      dictationBase = null;
      lastSettled = null;
      setProvisionalFrom(null);
      const committed = glued(base, text);
      options.write(committed);
      // Persist here, not via the reactive effect: an unmount-time commit runs
      // while that effect is already being torn down.
      options.persist(committed);
    },
    /** Explicit ✕: restore the pre-mic baseline — the only self-driven revert. */
    discard: () => {
      const base = dictationBase ?? options.draft();
      dictationBase = null;
      lastSettled = null;
      setProvisionalFrom(null);
      options.write(base);
      options.persist(base);
    },
    /** Send clears the field: the replaced tail no longer exists. */
    release: () => {
      dictationBase = null;
      lastSettled = null;
      setProvisionalFrom(null);
    },
    /** Where the unsettled hypothesis starts in the draft; null = none. */
    provisionalFrom,
    /** The user typed: the recorded split no longer describes the field. */
    clearProvisional: () => setProvisionalFrom(null),
  };
}
