// Determinate-meter model behind the loading card's attach-progress bar.
// Lives outside the .tsx because bun test resolves Solid components to their
// SSR build (no DOM), while this pure contract must stay unit-testable.
// Consumed by TerminalLoadingNotice; null means only the indeterminate
// animation applies.


export type TerminalProgressInput = {
  received: number;
  total: number;
} | null | undefined;

export interface TerminalProgressView {
  determinate: boolean;
  percent: number;
  label: string;
}

/** Model for an in-flight chunked baseline, or null when only the
 * indeterminate bar applies. Clamps so a racing chunk count can never push
 * the bar past its track. */
export function terminalLoadingProgressView(
  progress: TerminalProgressInput,
): TerminalProgressView | null {
  if (!progress || !Number.isFinite(progress.total) || progress.total <= 0) return null;
  const received = Math.min(Math.max(progress.received, 0), progress.total);
  const percent = Math.round((received / progress.total) * 100);
  return {
    determinate: true,
    percent,
    label: `${percent}% · part ${received}/${progress.total}`,
  };
}
