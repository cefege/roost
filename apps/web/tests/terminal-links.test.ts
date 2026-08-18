// Link detection for the terminal linkifier (computeRowLinks). Pure algorithm,
// no DOM — the applier that consumes these segments lives in terminal-links.ts.
//
// Two separate concerns are pinned here:
//   * SOFT-WRAP JOINING. A URL longer than the terminal width wraps across
//     visual rows, and per-row detection sees only partial strings that match
//     wrong or not at all; detection runs on the JOINED logical line instead.
//     The "did this row fill the grid?" test is in COLUMNS, never in UTF-16 code
//     units — a CJK ideograph is 2 columns / 1 unit and a ZWJ emoji cluster is
//     2 columns / 11 units, so a code-unit count both misses real joins and
//     fabricates fake ones.
//   * INFERRED links (regex URLs, GitHub refs, resolvable file paths) versus
//     PAINTED producer links. OSC 8 hyperlinks are authored by the core per cell
//     and painted as anchors by cellRow.ts; they reach this module only as
//     already-placed column ranges. Nothing here text-matches them, and a
//     painted link never comes back as a segment — the return value is what the
//     DOM still needs wrapped.

import { test, expect } from "bun:test";
import { computeRowLinks } from "../src/components/terminal-links.ts";
import type { PaintedLink, RowLinkInput } from "../src/components/terminal-links.ts";

/** Rows of NARROW characters — one column per UTF-16 code unit. Box-drawing
 *  borders (│) are narrow too, so the TUI fixtures below qualify. Rows with
 *  wide glyphs state their occupancy explicitly instead. */
const rows = (...texts: string[]): RowLinkInput[] =>
  texts.map((text) => ({ text, columns: text.length }));

/** One narrow row carrying painted producer links. */
const painted = (text: string, ...links: PaintedLink[]): RowLinkInput =>
  ({ text, columns: text.length, links });

const stubResolve = (p: string, line: number | null) =>
  `/file/W/${p.replace(/^\//, "")}${line ? `#L${line}` : ""}`;

// ── soft-wrap joining ─────────────────────────────────────────────────────
test("URL wrapped across two rows → one link, two per-row segments", () => {
  // cols=10: row0 fills the grid (10 columns) so it wraps into row1.
  const segs = computeRowLinks(rows("https://ex", "ample.com"), 10);
  expect(segs.length).toBe(2);
  expect(segs[0]).toEqual({ row: 0, start: 0, end: 10, url: "https://example.com" });
  expect(segs[1]).toEqual({ row: 1, start: 0, end: 9, url: "https://example.com" });
});

test("URL wrapped across three rows → three segments, same href", () => {
  const segs = computeRowLinks(rows("https://", "example.", "com/page"), 8);
  expect(segs.map(s => s.row)).toEqual([0, 1, 2]);
  expect(new Set(segs.map(s => s.url))).toEqual(new Set(["https://example.com/page"]));
  // Reassembling the segments' spans reconstructs the full URL.
  const joined = ["https://", "example.", "com/page"];
  expect(segs.map(s => joined[s.row].slice(s.start, s.end)).join("")).toBe("https://example.com/page");
});

test("cols<=0 (no grid width) → no join, per-row detection only", () => {
  // Without cols we can't know the row wrapped, so each row is scanned alone.
  const segs = computeRowLinks(rows("https://ex", "ample.com"), 0);
  // row1 "ample.com" has no scheme → no match; row0 matches the truncated URL.
  expect(segs.every(s => s.row === 0)).toBe(true);
});

test("single-row URL unaffected — correct offsets within the row", () => {
  const segs = computeRowLinks(rows("visit https://example.com now"), 80);
  expect(segs.length).toBe(1);
  expect(segs[0].row).toBe(0);
  expect(segs[0].start).toBe(6);
  expect(segs[0].end).toBe(6 + "https://example.com".length);
});

test("trailing sentence punctuation excluded from a wrapped URL", () => {
  // "https://example.com." wrapped at cols=10 — the lookbehind drops the dot.
  const segs = computeRowLinks(rows("https://ex", "ample.com."), 10);
  const reassembled = ["https://ex", "ample.com."];
  expect(segs.map(s => reassembled[s.row].slice(s.start, s.end)).join("")).toBe("https://example.com");
});

test("short line that does not fill the grid is not joined to the next", () => {
  const segs = computeRowLinks(rows("hello", "https://example.com"), 10);
  // "hello" (5 < 10) does not wrap; the URL is detected wholly on row 1.
  expect(segs.length).toBe(1);
  expect(segs[0].row).toBe(1);
  expect(segs[0].url).toBe("https://example.com");
});

// ── the join test is COLUMNS, not code units ──────────────────────────────
test("a wide-glyph row that fills the grid still joins — a code-unit count would not", () => {
  // 3 CJK ideographs (2 columns each) + "http" = exactly 10 of 10 columns, but
  // only 7 UTF-16 code units. Grouping on code units never fires here, so the
  // wrapped URL goes silently unlinkified — the undercount direction.
  const input: RowLinkInput[] = [
    { text: "中中中http", columns: 10 },
    { text: "://ex.co", columns: 8 },
  ];
  expect(input[0].text.length).toBe(7); // the count that would lose the link
  const segs = computeRowLinks(input, 10);
  expect(segs.map(s => s.url)).toEqual(["http://ex.co", "http://ex.co"]);
  expect(segs.map(s => input[s.row].text.slice(s.start, s.end)).join("")).toBe("http://ex.co");
});

test("an emoji-cluster row that does NOT fill the grid must not join", () => {
  // A ZWJ family emoji is 2 columns and 11 code units: this row occupies 6 of 8
  // columns but is 15 units long. Joining on units fuses it with the next,
  // unrelated row, and the fake boundary fabricates a file link out of two
  // halves that never belonged together — the overcount direction.
  const input: RowLinkInput[] = [
    { text: "\u{1F468}\u200D\u{1F469}\u200D\u{1F467}\u200D\u{1F466}x.ts", columns: 6 },
    { text: ":9", columns: 2 },
  ];
  expect(input[0].text.length).toBe(15); // the count that would fabricate a join
  expect(computeRowLinks(input, 8, stubResolve)).toEqual([]);
  // Same rows with occupancy misreported as code units: the bogus join returns.
  const asUnits = input.map(r => ({ text: r.text, columns: r.text.length }));
  const bogus = computeRowLinks(asUnits, 8, stubResolve);
  expect(bogus.some(s => s.kind === "file")).toBe(true);
});

// ── URL broken by TUI border chars at soft-wrap boundaries ────────────────
test("URL in bordered TUI panel (│ borders) → one link across rows", () => {
  // A TUI draws │ box-drawing borders flanking each visual row; a long URL
  // wraps so each row's text is `│ <chunk>│`. The non-URL │ must not break
  // detection — the border decoration is stripped at row boundaries before
  // joining, yielding one complete link.
  const lines = [
    "│ https://example.com/very/long/path/that/wraps│",
    "│ /across/rows/with/border/decoration│",
  ];
  const segs = computeRowLinks(rows(...lines), lines[0].length);
  expect(new Set(segs.map(s => s.url)).size).toBe(1);
  expect(segs[0].url).toBe(
    "https://example.com/very/long/path/that/wraps/across/rows/with/border/decoration",
  );
  // Per-row spans reassemble to the full URL (borders excluded).
  expect(segs.map(s => lines[s.row].slice(s.start, s.end)).join("")).toBe(segs[0].url);
});

test("blank bordered row between two URLs → two separate links", () => {
  // All three rows are full-width (cols) so they form one wrapped logical
  // line; the middle row is pure │ borders + spaces. Without the
  // empty-after-strip guard that blank row would vanish and the two URLs would
  // merge into one broken href — the guard keeps it verbatim as a separator.
  const lines = [
    "│ https://a.com/foo│",
    "│ https://b.com/bar│",
  ];
  const w = lines[0].length;
  const segs = computeRowLinks(rows(lines[0], "│" + " ".repeat(w - 2) + "│", lines[1]), w);
  expect(new Set(segs.map(s => s.url))).toEqual(
    new Set(["https://a.com/foo", "https://b.com/bar"]),
  );
});

// ── file paths (resolver-gated) ───────────────────────────────────────────
test("path with slash + :line → internal file link", () => {
  const segs = computeRowLinks(rows("Edited apps/web/src/FolderList.tsx:142 ok"), 80, stubResolve);
  const f = segs.find(s => s.kind === "file");
  expect(f?.url).toBe("/file/W/apps/web/src/FolderList.tsx#L142");
  expect(f?.hint).toBe("Open apps/web/src/FolderList.tsx:142");
});

test("absolute path resolves; version string does not", () => {
  expect(computeRowLinks(rows("see /Users/you/x.rs here"), 80, stubResolve)
    .some(s => s.kind === "file")).toBe(true);
  expect(computeRowLinks(rows("v1.2.3 shipped"), 80, stubResolve)
    .some(s => s.kind === "file")).toBe(false);
});

test("Windows drive, UNC, and backslash paths reach the resolver intact", () => {
  const seen: Array<{ path: string; line: number | null }> = [];
  const resolve = (path: string, line: number | null) => {
    seen.push({ path, line });
    return "/file/windows";
  };
  computeRowLinks(rows(
    String.raw`C:\Users\Ada\src\main.ts:42`,
    String.raw`\\server\share\logs\build.log:7`,
    "D:/work/roost/readme.md",
  ), 120, resolve);
  expect(seen).toEqual([
    { path: String.raw`C:\Users\Ada\src\main.ts`, line: 42 },
    { path: String.raw`\\server\share\logs\build.log`, line: 7 },
    { path: "D:/work/roost/readme.md", line: null },
  ]);
});

test("bare filename links only with a :line", () => {
  expect(computeRowLinks(rows("see FolderList.tsx here"), 80, stubResolve)
    .some(s => s.kind === "file")).toBe(false);
  expect(computeRowLinks(rows("see FolderList.tsx:9 here"), 80, stubResolve)
    .some(s => s.kind === "file")).toBe(true);
});

test("bare archive filenames link without a :line", () => {
  // zip
  expect(computeRowLinks(rows("see backup.zip here"), 80, stubResolve)
    .some(s => s.kind === "file")).toBe(true);
  // tar.gz
  expect(computeRowLinks(rows("archive.tar.gz ready"), 80, stubResolve)
    .some(s => s.kind === "file")).toBe(true);
  // non-archive bare still no-link
  expect(computeRowLinks(rows("see readme.txt here"), 80, stubResolve)
    .some(s => s.kind === "file")).toBe(false);
  // .zip as part of a longer non-archive ext is NOT matched (\b gate)
  expect(computeRowLinks(rows("data.csv here"), 80, stubResolve)
    .some(s => s.kind === "file")).toBe(false);
});

test("long paths in command-block output link correctly", () => {
  // Exact text from terminal command-block rows
  const cmdRow = "│ /Users/you/Code/project/docs/report_2026-07-13.zip                                 │";
  const segs = computeRowLinks(rows(cmdRow), 80, stubResolve);
  const fileLinks = segs.filter(s => s.kind === "file");
  expect(fileLinks.length).toBe(1);
  expect(fileLinks[0].url).toBe("/file/W/Users/you/Code/project/docs/report_2026-07-13.zip");
});

test("scheme URLs are NOT evicted by overlapping file-path matches", () => {
  // A path-like segment inside a URL (example.com/path/file.zip) should NOT
  // nuke the valid https:// link. Only file:// links are evicted.
  const segs = computeRowLinks(rows("see https://example.com/path/file.zip here"), 80, stubResolve);
  // URL link preserved
  expect(segs.some(s => s.kind === undefined && s.url === "https://example.com/path/file.zip")).toBe(true);
  // No file link for the path-like substring
  expect(segs.some(s => s.kind === "file")).toBe(false);
});

test("~-prefixed paths are matched whole, not partially as /absolute", () => {
  // ~ is not in [\w.@\-], so without the ~ prefix in branch A, the regex would
  // start at the / after ~, creating a broken "absolute" path /Code/... instead
  // of the correct ~/Code/... . With the ~ prefix, the full ~/ path is matched
  // and resolveFile returns null for ~ paths (no link, but no broken link either).
  const segs = computeRowLinks(rows("see ~/Code/proj/a.py here"), 80, stubResolve);
  const fileLinks = segs.filter(s => s.kind === "file");
  expect(fileLinks.length).toBe(1);
  // The path starts with ~ (not /)
  expect(fileLinks[0].url).toContain("~/Code/proj/a.py");
});

test("~ paths are not linkified when resolver rejects them", () => {
  // Real resolveFile returns null for ~ paths (can't expand home dir client-side).
  const realResolve = (p: string, _line: number | null) =>
    p.startsWith("~") ? null : `/file/W/${p.replace(/^\//, "")}`;
  expect(computeRowLinks(rows("see ~/Code/proj/a.py here"), 80, realResolve)
    .filter(s => s.kind === "file").length).toBe(0);
  // A non-~ path with the same resolver still works
  expect(computeRowLinks(rows("see /Users/you/a.py here"), 80, realResolve)
    .filter(s => s.kind === "file").length).toBe(1);
});

test("no resolver → no file links (URL-only behavior preserved)", () => {
  expect(computeRowLinks(rows("apps/web/foo.ts:1"), 80)).toEqual([]);
});

// ── localhost dev URLs ───────────────────────────────────────────────────
test("localhost:PORT becomes an http url; bare localhost does not", () => {
  const segs = computeRowLinks(rows("Local: localhost:5174/app now"), 80);
  expect(segs.find(s => s.url.startsWith("http://localhost"))?.url).toBe("http://localhost:5174/app");
  expect(computeRowLinks(rows("run on localhost soon"), 80).length).toBe(0);
});

// ── GitHub refs ──────────────────────────────────────────────────────────
test("owner/repo#123 → issues url (self-contained)", () => {
  const segs = computeRowLinks(rows("fixes owner/repo#412 today"), 80);
  expect(segs.find(s => s.url.includes("github.com"))?.url)
    .toBe("https://github.com/owner/repo/issues/412");
});

test("owner/repo@sha → commit url (self-contained)", () => {
  const segs = computeRowLinks(rows("at owner/repo@deadbeef now"), 80);
  expect(segs.find(s => s.url.includes("/commit/"))?.url)
    .toBe("https://github.com/owner/repo/commit/deadbeef");
});

test("bare #N and commit SHA link only when the session repo is known", () => {
  expect(computeRowLinks(rows("see #7 and a1b2c3d4 here"), 80)).toEqual([]);
  const urls = computeRowLinks(rows("see #7 and a1b2c3d4 here"), 80, undefined, "o/r").map(s => s.url);
  expect(urls).toContain("https://github.com/o/r/issues/7");
  expect(urls).toContain("https://github.com/o/r/commit/a1b2c3d4");
});

test("a pure-number token is not treated as a commit SHA", () => {
  const segs = computeRowLinks(rows("build 1234567 done"), 80, undefined, "o/r");
  expect(segs.some(s => s.url.includes("/commit/"))).toBe(false);
});

// ── painted producer links (OSC 8) ───────────────────────────────────────
// The core authors these per cell and cellRow.ts has already painted them as
// anchors. They arrive as column ranges purely so the inferred passes can respect
// them, and they are never returned as work.
const PAINTED_KEY = "b\u00007";

test("a painted producer URI wins its span over the identical regex URL", () => {
  // The producer hyperlinked the whole visible URL but pointed it elsewhere. The
  // painted anchor keeps that destination: the identical-span regex match is
  // dropped, so the applier is told to wrap nothing.
  const text = "https://github.com/cefege/roost";
  const segs = computeRowLinks(
    [painted(text, { start: 0, end: text.length, uri: `${text}/?custom=1`, key: PAINTED_KEY })],
    80,
  );
  expect(segs).toEqual([]);
  // Without the painted anchor the same row DOES need the inferred URL wrapped.
  expect(computeRowLinks(rows(text), 80))
    .toEqual([{ row: 0, start: 0, end: text.length, url: text }]);
});

test("a painted file: link on the filename does not block the full-path file link", () => {
  // `ls --hyperlink` paints an anchor on just the filename while the full path is
  // on screen. file:// is useless in a browser, so the resolvable path wins and
  // the painted anchor is the one the applier dissolves.
  const text = "/Users/you/Code/project/docs/report_2026-07-13.zip";
  const at = text.indexOf("report_2026-07-13.zip");
  const segs = computeRowLinks(
    [painted(text, { start: at, end: text.length, uri: `file://${text}`, key: PAINTED_KEY })],
    80,
    stubResolve,
  );
  expect(segs).toEqual([{
    row: 0,
    start: 0,
    end: text.length,
    url: "/file/W/Users/you/Code/project/docs/report_2026-07-13.zip",
    kind: "file",
    hint: `Open ${text}`,
  }]);
});

test("a painted link soft-wrapped across rows is ONE match, replaced as one link", () => {
  // cols=10: row0 fills the grid, so the anchor's two halves are adjacent in the
  // joined line and share the core's run key. They fuse, so the resolvable path
  // covering BOTH halves evicts the whole file:// link instead of half of it —
  // and the applier gets one segment per row, same href.
  const half = (start: number, end: number): PaintedLink =>
    ({ start, end, uri: "file:///tmp/Foo.txt", key: PAINTED_KEY });
  const input: RowLinkInput[] = [
    { text: "see /tmp/F", columns: 10, links: [half(9, 10)] },
    { text: "oo.txt", columns: 6, links: [half(0, 6)] },
  ];
  const segs = computeRowLinks(input, 10, stubResolve);
  expect(segs.map(s => s.kind)).toEqual(["file", "file"]);
  expect(new Set(segs.map(s => s.url))).toEqual(new Set(["/file/W/tmp/Foo.txt"]));
  expect(segs.map(s => input[s.row].text.slice(s.start, s.end)).join("")).toBe("/tmp/Foo.txt");
});

// ── ROW_LINK_HINT prefilter — one case per pattern family ─────────────────
// computeRowLinks skips the whole regex battery for a logical line with no hint
// character. A family that needs a character the hint does not test would go
// silently undetected, so each family is pinned here. A NEW pattern MUST arrive
// with its own case.
const resolveFile = (path: string, line: number | null): string =>
  `/file/fp/${path}${line === null ? "" : `#L${line}`}`;

test("prefilter keeps every link family detectable", () => {
  const one = (texts: string[], ownerRepo?: string): string[] =>
    computeRowLinks(rows(...texts), 80, resolveFile, ownerRepo).map((s) => s.url);

  expect(one(["open https://example.com now"])).toEqual(["https://example.com"]);
  expect(one(["mail me at mailto:a@b.co"])).toEqual(["mailto:a@b.co"]);
  expect(one(["Local:   localhost:5174/"])).toEqual(["http://localhost:5174/"]);
  expect(one(["at apps/web/src/foo.ts:42 exactly"])).toEqual(["/file/fp/apps/web/src/foo.ts#L42"]);
  expect(one(["see foo.ts:9 there"])).toEqual(["/file/fp/foo.ts#L9"]);
  expect(one(["grab release.tar.gz please"])).toEqual(["/file/fp/release.tar.gz"]);
  expect(one(["fixed cefege/roost#12 today"])).toEqual(["https://github.com/cefege/roost/issues/12"]);
  expect(one(["landed cefege/roost@deadbeef ok"])).toEqual(["https://github.com/cefege/roost/commit/deadbeef"]);
  expect(one(["closes #77 finally"], "cefege/roost")).toEqual(["https://github.com/cefege/roost/issues/77"]);
  // The bare-SHA family carries NONE of : / . # — the hint's hex-run branch is
  // the only thing that keeps it detectable.
  expect(one(["reverted deadbeef1 earlier"], "cefege/roost"))
    .toEqual(["https://github.com/cefege/roost/commit/deadbeef1"]);
});

test("prefilter skips a line that cannot contain any link", () => {
  // Prose with no hint character at all: nothing to find, and nothing is found.
  expect(computeRowLinks(rows("the quick brown fox jumps over a lazy dog"), 80, resolveFile, "cefege/roost"))
    .toEqual([]);
});

test("prefilter skips a hint-free line and leaves its painted anchor alone", () => {
  // Producer link text is arbitrary — here it carries no hint character at all.
  // With no hint character no pattern can match, so nothing could contest the
  // painted anchor and there is nothing to return. (The old text-matching
  // linkifier had to disable the prefilter outright for exactly this case.)
  expect(computeRowLinks(
    [painted("open Readme now", { start: 5, end: 11, uri: "https://x.test/readme", key: PAINTED_KEY })],
    80,
  )).toEqual([]);
});
