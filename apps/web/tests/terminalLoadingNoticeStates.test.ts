// Loading-card attach-progress meter states. This repo runs no jsdom and
// Solid resolves to its SSR build under `bun test` (see
// folderListRowStability.dom.test.ts), so the component itself cannot render
// here; the observable meter contract is locked through
// terminalLoadingProgressView — the pure model TerminalLoadingNotice renders,
// where a non-null view switches the card from its indeterminate animated bar
// to the determinate "% · part n/m" readout, and any falsy progress keeps the
// indeterminate fallback.

import { describe, expect, test } from "bun:test";
import { terminalLoadingProgressView } from "../src/lib/terminalLoadingProgress.ts";

describe("terminalLoadingProgressView", () => {
  test("determinate state: mid-assembly chunked baseline", () => {
    expect(terminalLoadingProgressView({ received: 3, total: 7 })).toEqual({
      determinate: true,
      percent: 43,
      label: "43% · part 3/7",
    });
  });

  test("complete baseline reads exactly full", () => {
    expect(terminalLoadingProgressView({ received: 4, total: 4 })).toEqual({
      determinate: true,
      percent: 100,
      label: "100% · part 4/4",
    });
  });

  test("indeterminate state: no chunk assembly in flight", () => {
    // Single-frame baselines, scrollback backfill, and idle replicas all pass
    // one of these shapes; every one must keep the indeterminate bar.
    expect(terminalLoadingProgressView(null)).toBeNull();
    expect(terminalLoadingProgressView(undefined)).toBeNull();
    expect(terminalLoadingProgressView({ received: 0, total: 0 })).toBeNull();
  });

  test("a racing chunk count never pushes past the track", () => {
    const overrun = terminalLoadingProgressView({ received: 9, total: 7 });
    expect(overrun).toEqual({
      determinate: true,
      percent: 100,
      label: "100% · part 7/7",
    });
    const negative = terminalLoadingProgressView({ received: -2, total: 5 });
    expect(negative?.percent).toBe(0);
    expect(negative?.label).toBe("0% · part 0/5");
  });

  test("non-finite totals stay indeterminate instead of NaN-labeling", () => {
    expect(
      terminalLoadingProgressView({
        received: 1,
        total: Number.NaN,
      }),
    ).toBeNull();
  });
});
