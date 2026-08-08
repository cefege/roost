// composerDrafts — the mobile composer's unsent text, retained per session on
// this device. Solid components can't be rendered under bun test in this repo
// (SSR build; see the note in folderListRowStability.dom.test.ts), so this
// covers the storage module directly: round-trip, per-session isolation, ""
// removing an entry, exact whitespace, the localStorage blob (the reload path),
// and the LRU-by-write cap.

import { expect, test, describe, beforeEach } from "bun:test";

// bun test has no localStorage — stub before importing the module under test.
const _ls: Record<string, string> = {};
(globalThis as unknown as { localStorage: Storage }).localStorage = {
  getItem: (k: string) => _ls[k] ?? null,
  setItem: (k: string, v: string) => { _ls[k] = v; },
  removeItem: (k: string) => { delete _ls[k]; },
  clear: () => { for (const k of Object.keys(_ls)) delete _ls[k]; },
  key: () => null, length: 0,
} as Storage;

// Dynamic: the module reads storage at import time, so the stub must be in place.
const { getComposerDraft, saveComposerDraft } = await import("../src/lib/composerDrafts.ts");

const KEY = "roost.composerDrafts.v1";
const stored = () => JSON.parse(localStorage.getItem(KEY) ?? "{}") as Record<string, string>;

describe("composerDrafts", () => {
  // The module's map is loaded once at import, so each test clears through the
  // public writer rather than localStorage.clear() (which would desync them).
  beforeEach(() => {
    for (const id of Object.keys(stored())) saveComposerDraft(id, "");
  });

  test("round-trips a draft; an unknown session has none", () => {
    saveComposerDraft("a", "hi");
    expect(getComposerDraft("a")).toBe("hi");
    expect(getComposerDraft("never-typed")).toBe("");
  });

  test("drafts are isolated per session", () => {
    saveComposerDraft("a", "for a");
    saveComposerDraft("b", "for b");
    expect(getComposerDraft("a")).toBe("for a");
    expect(getComposerDraft("b")).toBe("for b");
  });

  test('saving "" removes the entry', () => {
    saveComposerDraft("a", "hi");
    saveComposerDraft("a", "");
    expect(getComposerDraft("a")).toBe("");
    expect(stored()).not.toHaveProperty("a");
  });

  test("whitespace is preserved exactly — spaces are real PTY input", () => {
    saveComposerDraft("a", "  x  ");
    expect(getComposerDraft("a")).toBe("  x  ");
    expect(stored().a).toBe("  x  ");
  });

  test("persists to localStorage so a reload restores the draft", () => {
    saveComposerDraft("a", "half typed message");
    expect(stored()).toEqual({ a: "half typed message" });
  });

  test("caps at 24 entries, dropping the oldest write", () => {
    for (let i = 0; i < 25; i++) saveComposerDraft(`s${i}`, `draft ${i}`);
    expect(Object.keys(stored())).toHaveLength(24);
    expect(getComposerDraft("s0")).toBe("");
    expect(getComposerDraft("s1")).toBe("draft 1");
    expect(getComposerDraft("s24")).toBe("draft 24");
  });

  test("a rewrite refreshes an entry's position in the cap", () => {
    for (let i = 0; i < 24; i++) saveComposerDraft(`s${i}`, `draft ${i}`);
    saveComposerDraft("s0", "still here");
    saveComposerDraft("s99", "newcomer");
    expect(getComposerDraft("s0")).toBe("still here");
    expect(getComposerDraft("s1")).toBe(""); // oldest write once s0 moved up
  });
});
