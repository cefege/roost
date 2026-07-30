// FOCUS_OWNERS tripwire for CellTerminal's document-level focus guards.
// Material inputs need explicit host selectors because shadow-root clicks are
// retargeted to the custom element. This repo has no jsdom, so the contract is
// checked as a selector string.

import { describe, test, expect } from "bun:test";
import { FOCUS_OWNERS } from "../src/lib/focusOwners.ts";

describe("FOCUS_OWNERS", () => {
  test("covers native text widgets", () => {
    for (const sel of ["input", "textarea", "select", '[role="textbox"]', '[role="dialog"]']) {
      expect(FOCUS_OWNERS).toContain(sel);
    }
  });

  test("covers Material custom-element HOSTS, which is what a shadow click retargets to", () => {
    // Without these a click on the field's inner textarea is seen as the host
    // tag, matches nothing, and the guard eats focus.
    for (const tag of ["md-outlined-text-field", "md-filled-text-field"]) {
      expect(FOCUS_OWNERS).toContain(tag);
    }
  });

  test("still lets bare chrome keep terminal focus", () => {
    // The guard's whole purpose: clicking a bare button (sidebar ✕, FAB, tabs)
    // must NOT move focus off the terminal. A blanket "button" entry would
    // silently disable that.
    expect(FOCUS_OWNERS).not.toMatch(/(^|,)\s*button\s*(,|$)/);
    expect(FOCUS_OWNERS).not.toMatch(/(^|,)\s*\*\s*(,|$)/);
  });
});
