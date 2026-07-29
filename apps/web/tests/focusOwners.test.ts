// FOCUS_OWNERS tripwire.
//
// CellTerminal installs two document-level guards that keep keyboard focus on
// the terminal: a mousedown PREVENT and a keydown RECOVER. Both decide with
// `target.closest(FOCUS_OWNERS)`. A click inside a SHADOW ROOT retargets
// e.target to the host element, so a Material text field is seen as
// <md-outlined-text-field> — which matched none of the original selectors.
//
// Consequence, shipped once: the agent composer could not be focused at all.
// On desktop there was no caret and keystrokes were force-routed to the PTY; on
// mobile the keyboard never opened, because the guard called preventDefault on
// the tap. Verified fixed in a real browser (click + real key events, and a
// touch tap on a mobile viewport → document.activeElement is the textarea).
//
// This repo runs no jsdom by design (see cellRenderer.dom.test.ts), so this is a
// string tripwire rather than a DOM test: it fails if someone narrows the
// allowlist back down, which is the regression that actually happened.

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

  test("covers the agent transcript wholesale", () => {
    // TranscriptDeck renders ABOVE a still-mounted TerminalDeck, so nothing
    // inside it is ever the terminal's focus to keep — including controls added
    // to the transcript later, which is why this is a subtree match.
    expect(FOCUS_OWNERS).toContain('[data-testid="transcript-deck"]');
  });

  test("still lets bare chrome keep terminal focus", () => {
    // The guard's whole purpose: clicking a bare button (sidebar ✕, FAB, tabs)
    // must NOT move focus off the terminal. A blanket "button" entry would
    // silently disable that.
    expect(FOCUS_OWNERS).not.toMatch(/(^|,)\s*button\s*(,|$)/);
    expect(FOCUS_OWNERS).not.toMatch(/(^|,)\s*\*\s*(,|$)/);
  });
});
