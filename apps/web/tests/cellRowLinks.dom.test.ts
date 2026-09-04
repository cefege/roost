// OSC 8 hyperlinks in the paint (CellSpan.linkUri / linkKey).
//
// Links are CORE-AUTHORED: @wterm/core resolves each cell's OSC 8 link index to
// a URI and a run key, the wire carries them per span, and renderRow paints the
// anchor at exactly those cells. Nothing derives links from the byte stream and
// nothing matches link TEXT, so the two failure modes the old text-matching
// linkifier had are structurally gone: a link can no longer appear on identical
// text somewhere else, and two links with the same visible text keep their own
// URIs. What CAN still break is the paint:
//   * coalescing two differently-linked runs into one anchor (one URI wins, the
//     other silently disappears);
//   * splitting a linked run at a find-hit boundary and losing the link on one
//     half;
//   * painting a producer-controlled `javascript:` URI into a clickable href.
//
// No jsdom (repo convention, see cellRenderer.dom.test.ts): a small fake covers
// exactly what renderRow touches.

import { describe, test, expect } from "bun:test";
import {
  renderRow, rowHash,
  LINK_KEY_ATTR, ROW_COLUMNS_ATTR, ROW_HAS_LINKS_ATTR, TERMINAL_LINK_CLASS,
} from "../src/lib/cellRow.ts";
import type { FindHit } from "../src/lib/cellRow.ts";
import { DEFAULT_COLOR, type CellRow, type CellSpan } from "@roost/shared/cell";

// ── minimal fake DOM ──────────────────────────────────────────────────────
class FakeEl {
  className = "";
  textContent = "";
  children: FakeEl[] = [];
  attrs: Record<string, string> = {};
  constructor(public tagName: string) {}
  setAttribute(key: string, value: string): void { this.attrs[key] = value; }
  appendChild(child: FakeEl): FakeEl { this.children.push(child); return child; }
}
const fakeDoc = {
  createElement: (tag: string) => new FakeEl(tag),
  createTextNode: (text: string) => {
    const node = new FakeEl("#text");
    node.textContent = text;
    return node;
  },
} as unknown as Document;

const paint = (spans: CellSpan[], hits?: readonly FindHit[], activeCol?: number): FakeEl =>
  renderRow({ index: 0, spans }, fakeDoc, hits, activeCol) as unknown as FakeEl;

/** A coalesced narrow run, shaped exactly as rowToSpans emits it. */
function run(text: string, over: Partial<CellSpan> = {}): CellSpan {
  return {
    text, columns: text.length,
    fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0, fgRgb: undefined, bgRgb: undefined,
    ...over,
  };
}
/** A run carrying a producer link. `key` is the core's per-instance run id. */
const linked = (text: string, uri: string, key: string): CellSpan =>
  run(text, { linkUri: uri, linkKey: key });

const anchorsOf = (row: FakeEl): FakeEl[] => row.children.filter((c) => c.tagName === "a");
/** Every painted child's text, anchors included, in row order. */
const flatText = (row: FakeEl): string =>
  row.children.map((c) => (c.tagName === "a" ? c.children.map((g) => g.textContent).join("") : c.textContent)).join("");

describe("painted OSC 8 anchors", () => {
  test("a linked span paints one anchor carrying that exact URI", () => {
    const row = paint([run("see "), linked("Foo.txt", "https://ex.test/foo?a=1&b=2", "e\u0000id7\u0000x")]);
    const anchors = anchorsOf(row);
    expect(anchors.length).toBe(1);
    // Verbatim, not a resolved-then-serialized href: the linkifier compares this
    // string, and a rewritten URI would retarget the click.
    expect(anchors[0].attrs.href).toBe("https://ex.test/foo?a=1&b=2");
    expect(anchors[0].className).toBe(TERMINAL_LINK_CLASS);
    expect(anchors[0].attrs[LINK_KEY_ATTR]).toBe("e\u0000id7\u0000x");
    expect(anchors[0].attrs.target).toBe("_blank");
    expect(anchors[0].attrs.rel).toBe("noopener noreferrer");
    expect(anchors[0].attrs["data-hint"]).toBe("https://ex.test/foo?a=1&b=2");
    // The linked cells live INSIDE the anchor; unlinked text stays outside it.
    expect(anchors[0].children.map((c) => c.textContent)).toEqual(["Foo.txt"]);
    expect(flatText(row)).toBe("see Foo.txt");
  });

  test("two adjacent spans with different linkKey paint two anchors", () => {
    // The whole point of the cutover: identical visible text, different targets.
    // A text-matching linkifier cannot tell these apart; per-cell identity can.
    const row = paint([
      linked("report", "https://ex.test/one", "b\u00000"),
      linked("report", "https://ex.test/two", "b\u00001"),
    ]);
    const anchors = anchorsOf(row);
    expect(anchors.map((a) => a.attrs.href)).toEqual(["https://ex.test/one", "https://ex.test/two"]);
    expect(anchors.map((a) => a.attrs[LINK_KEY_ATTR])).toEqual(["b\u00000", "b\u00001"]);
    expect(flatText(row)).toBe("reportreport");
  });

  test("adjacent spans sharing a linkKey paint one anchor", () => {
    // A style change inside one hyperlink splits the span but not the link.
    const row = paint([
      linked("bold", "https://ex.test/x", "b\u00002"),
      linked("plain", "https://ex.test/x", "b\u00002"),
    ]);
    const anchors = anchorsOf(row);
    expect(anchors.length).toBe(1);
    expect(anchors[0].children.map((c) => c.textContent)).toEqual(["bold", "plain"]);
  });

  test("unlinked cells between two same-key runs end the anchor", () => {
    const row = paint([
      linked("a", "https://ex.test/x", "b\u00003"),
      run(" gap "),
      linked("b", "https://ex.test/x", "b\u00003"),
    ]);
    expect(anchorsOf(row).length).toBe(2);
    expect(row.children.map((c) => c.tagName)).toEqual(["a", "span", "a"]);
    expect(flatText(row)).toBe("a gap b");
  });

  test("a find hit inside a link keeps both halves inside the one anchor", () => {
    // renderRow splits a run at hit boundaries. The split pieces must stay in the
    // anchor, or the highlighted half stops being clickable.
    const row = paint([linked("abcdef", "https://ex.test/hit", "b\u00004")], [{ col: 2, len: 2 }]);
    const anchors = anchorsOf(row);
    expect(anchors.length).toBe(1);
    expect(anchors[0].children.map((c) => c.textContent)).toEqual(["ab", "cd", "ef"]);
    expect(anchors[0].children.map((c) => c.className)).toEqual(["", "cell-find-hit", ""]);
    expect(flatText(row)).toBe("abcdef");
  });

  test("only HTTP(S) and worker-file targets paint anchors", () => {
    for (const uri of [
      "javascript:alert(1)",
      "  https://space.invalid",
      "data:text/html,<b>",
      "vbscript:x",
      "vscode://file/a.ts",
      "ssh://host/a.ts",
      "//protocol-relative.invalid/a.ts",
    ]) {
      const row = paint([run("prefix "), linked("click me", uri, "b\u00005")]);
      expect(anchorsOf(row).length).toBe(0);
      expect(flatText(row)).toBe("prefix click me");
    }
    const file = anchorsOf(paint([linked("source", "file:///tmp/a.ts#L9", "b\u00006")]))[0];
    expect(file.attrs.href).toBeUndefined();
    expect(file.attrs["data-kind"]).toBe("file");
    expect(file.attrs["data-terminal-target"]).toBe("file:///tmp/a.ts#L9");
  });

  test("the row link marker is present exactly when an anchor was painted", () => {
    // terminal-links.ts gates its whole painted-link read on this one attribute,
    // so a false negative silently blinds the inferred passes to a producer link
    // (a file:// anchor then survives a resolvable path that should replace it),
    // and a false positive costs a child walk on every ordinary row of a scan.
    expect(paint([linked("x", "https://ex.test/x", "b\u00008")]).attrs[ROW_HAS_LINKS_ATTR]).toBe("1");
    expect(paint([run("plain text")]).attrs[ROW_HAS_LINKS_ATTR]).toBeUndefined();
    expect(paint([]).attrs[ROW_HAS_LINKS_ATTR]).toBeUndefined();
    // A refused scheme paints no anchor, so it must not claim one either.
    expect(paint([linked("x", "javascript:alert(1)", "b\u00009")]).attrs[ROW_HAS_LINKS_ATTR])
      .toBeUndefined();
  });
});

describe("row grid-occupancy stamp", () => {
  test("the stamp is COLUMNS, not code units", () => {
    // Soft-wrap grouping in terminal-links.ts reads this back to decide whether a
    // row filled the grid. A CJK ideograph is 2 columns / 1 code unit and a ZWJ
    // family emoji 2 columns / 11, so a code-unit count both misses real joins
    // and fabricates fake ones. renderRow is the only place that still knows.
    const cjk = paint([run("中中中", { columns: 6 }), run("http")]);
    expect(cjk.attrs[ROW_COLUMNS_ATTR]).toBe("10");
    expect(flatText(cjk).length).toBe(7);

    const cluster = paint([run("\u{1F468}\u200D\u{1F469}\u200D\u{1F467}\u200D\u{1F466}", { columns: 2 }), run("x.ts")]);
    expect(cluster.attrs[ROW_COLUMNS_ATTR]).toBe("6");
    expect(flatText(cluster).length).toBe(15);
  });

  test("a blank row stamps zero columns", () => {
    // It still paints a space so the line box keeps its height — that space must
    // never read as one occupied column.
    const blank = paint([]);
    expect(blank.attrs[ROW_COLUMNS_ATTR]).toBe("0");
    expect(blank.children.map((c) => c.textContent)).toEqual([" "]);
  });
});

describe("rowHash sees link identity", () => {
  const r = (spans: CellSpan[]): CellRow => ({ index: 0, spans });
  test("same text and style, different link → different hash", () => {
    // Without this the viewport diff skips the repaint and the row keeps painting
    // yesterday's href.
    expect(rowHash(r([linked("report", "https://ex.test/one", "b\u00000")])))
      .not.toBe(rowHash(r([linked("report", "https://ex.test/two", "b\u00001")])));
  });
  test("gaining a link changes the hash; an identical link does not", () => {
    expect(rowHash(r([run("report")])))
      .not.toBe(rowHash(r([linked("report", "https://ex.test/one", "b\u00000")])));
    expect(rowHash(r([linked("report", "https://ex.test/one", "b\u00000")])))
      .toBe(rowHash(r([linked("report", "https://ex.test/one", "b\u00000")])));
  });
});
