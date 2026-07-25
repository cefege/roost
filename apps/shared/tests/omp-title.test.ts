// omp OSC-title identity. This predicate rotted silently in THREE places at
// once (worker chat gate, web chat toggle, omp detection manifest) because all
// three anchored on `π:` — a form omp only emits when tui.titleState is off,
// and that setting defaults to TRUE. So on a stock install the real titles were
// `π > label` / `π ⠋ label` / `π ! label` and every gate was wrong: the chat
// toggle vanished the moment the agent started working, and terminal omp panes
// got no status badge at all.
//
// The strings below are verbatim from omp 17.1.2
// (utils/title-generator.ts::buildTerminalTitleWithState) and from titles
// captured live off a running fleet.

import { test, expect } from "bun:test";
import { isOmpTitle } from "../src/chat/omp-title.ts";

test("every run state omp emits is omp", () => {
  expect(isOmpTitle("\u03C0 > Fix the divider drag")).toBe(true);   // idle
  expect(isOmpTitle("\u03C0 ! Fix the divider drag")).toBe(true);   // blocked on user
  expect(isOmpTitle("\u03C0: Fix the divider drag")).toBe(true);    // tui.titleState off
});

test("every spinner frame is omp — this is the one that broke the toggle", () => {
  // TITLE_SPINNER_FRAMES, animated at 80ms while the agent works.
  for (const f of ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]) {
    expect(isOmpTitle(`\u03C0 ${f} Terminal scroll rewrite`)).toBe(true);
  }
});

test("captured live from a working fleet", () => {
  expect(isOmpTitle("\u03C0 ⠸ Fix TypeScript command directory structu")).toBe(true);
  expect(isOmpTitle("\u03C0 ⠋ Design pass on new chat UI for material")).toBe(true);
});

test("a bare brand with no label still carries state", () => {
  expect(isOmpTitle("\u03C0 >")).toBe(true);
  expect(isOmpTitle("\u03C0 ⠏")).toBe(true);
});

test("pi is NOT omp — its title is the brand plus space-dash", () => {
  expect(isOmpTitle("\u03C0 - ~/Code/idea")).toBe(false);
  expect(isOmpTitle("\u03C0 - some/dir")).toBe(false);
});

test("non-omp titles stay out", () => {
  expect(isOmpTitle("zsh")).toBe(false);
  expect(isOmpTitle("~/Code/idea")).toBe(false);
  expect(isOmpTitle("\u03C0")).toBe(false);        // brand alone, no separator
  expect(isOmpTitle("\u03C0x label")).toBe(false); // brand must be followed by a separator
  expect(isOmpTitle(undefined)).toBe(false);
  expect(isOmpTitle(null)).toBe(false);
  expect(isOmpTitle("")).toBe(false);
});
