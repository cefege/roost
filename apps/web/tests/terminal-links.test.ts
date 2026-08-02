// Regression tests for wrapped-URL link detection (computeRowLinks). The bug:
// a URL longer than the terminal width wraps across visual rows, and per-row
// detection sees only partial strings that match wrong or not at all. The fix
// detects on the JOINED logical line. Pure algorithm, no DOM — see
// terminal-links.ts::computeRowLinks.

import { test, expect } from "bun:test";
import { computeRowLinks } from "../src/components/terminal-links.ts";

test("URL wrapped across two rows → one link, two per-row segments", () => {
  // cols=10: row0 fills the grid (10 chars) so it wraps into row1.
  const segs = computeRowLinks(["https://ex", "ample.com"], 10);
  expect(segs.length).toBe(2);
  expect(segs[0]).toEqual({ row: 0, start: 0, end: 10, url: "https://example.com" });
  expect(segs[1]).toEqual({ row: 1, start: 0, end: 9, url: "https://example.com" });
});

test("URL wrapped across three rows → three segments, same href", () => {
  const segs = computeRowLinks(["https://", "example.", "com/page"], 8);
  expect(segs.map(s => s.row)).toEqual([0, 1, 2]);
  expect(new Set(segs.map(s => s.url))).toEqual(new Set(["https://example.com/page"]));
  // Reassembling the segments' spans reconstructs the full URL.
  const joined = ["https://", "example.", "com/page"];
  expect(segs.map(s => joined[s.row].slice(s.start, s.end)).join("")).toBe("https://example.com/page");
});

test("cols<=0 (no grid width) → no join, per-row detection only", () => {
  // Legacy byte renderer path: without cols we can't know the row wrapped, so
  // each row is scanned alone (prior behavior — the partial matches this fix
  // supersedes only when cols is known).
  const segs = computeRowLinks(["https://ex", "ample.com"], 0);
  // row1 "ample.com" has no scheme → no match; row0 matches the truncated URL.
  expect(segs.every(s => s.row === 0)).toBe(true);
});

test("single-row URL unaffected — correct offsets within the row", () => {
  const segs = computeRowLinks(["visit https://example.com now"], 80);
  expect(segs.length).toBe(1);
  expect(segs[0].row).toBe(0);
  expect(segs[0].start).toBe(6);
  expect(segs[0].end).toBe(6 + "https://example.com".length);
});

test("trailing sentence punctuation excluded from a wrapped URL", () => {
  // "https://example.com." wrapped at cols=10 — the lookbehind drops the dot.
  const segs = computeRowLinks(["https://ex", "ample.com."], 10);
  const reassembled = ["https://ex", "ample.com."];
  expect(segs.map(s => reassembled[s.row].slice(s.start, s.end)).join("")).toBe("https://example.com");
});

test("short line that does not fill the grid is not joined to the next", () => {
  const segs = computeRowLinks(["hello", "https://example.com"], 10);
  // "hello" (5 < 10) does not wrap; the URL is detected wholly on row 1.
  expect(segs.length).toBe(1);
  expect(segs[0].row).toBe(1);
  expect(segs[0].url).toBe("https://example.com");
});

test("OSC 8 link text wrapped across rows resolves to its hidden URI", () => {
  const osc8: Array<[string, string]> = [["Foo.txt", "file:///tmp/Foo.txt"]];
  const segs = computeRowLinks(["prefix Foo", ".txt"], 10, osc8);
  expect(new Set(segs.map(s => s.url))).toEqual(new Set(["file:///tmp/Foo.txt"]));
  const joined = ["prefix Foo", ".txt"];
  expect(segs.map(s => joined[s.row].slice(s.start, s.end)).join("")).toBe("Foo.txt");
});

// --- URL broken by TUI border chars at soft-wrap boundaries ---
test("URL in bordered TUI panel (│ borders) → one link across rows", () => {
  // A TUI draws │ box-drawing borders flanking each visual row; a long URL
  // wraps so each row's text is `│ <chunk>│`. The non-URL │ must not break
  // detection — the border decoration is stripped at row boundaries before
  // joining, yielding one complete link.
  const rows = [
    "│ https://example.com/very/long/path/that/wraps│",
    "│ /across/rows/with/border/decoration│",
  ];
  const segs = computeRowLinks(rows, rows[0].length);
  expect(new Set(segs.map(s => s.url)).size).toBe(1);
  expect(segs[0].url).toBe(
    "https://example.com/very/long/path/that/wraps/across/rows/with/border/decoration",
  );
  // Per-row spans reassemble to the full URL (borders excluded).
  expect(segs.map(s => rows[s.row].slice(s.start, s.end)).join("")).toBe(segs[0].url);
});

test("blank bordered row between two URLs → two separate links", () => {
  // All three rows are full-width (cols) so they form one wrapped logical
  // line; the middle row is pure │ borders + spaces. Without the
  // empty-after-strip guard that blank row would vanish and the two URLs would
  // merge into one broken href — the guard keeps it verbatim as a separator.
  const rows = [
    "│ https://a.com/foo│",
    "│ https://b.com/bar│",
  ];
  const w = rows[0].length;
  const input = [rows[0], "│" + " ".repeat(w - 2) + "│", rows[1]];
  const segs = computeRowLinks(input, w);
  expect(new Set(segs.map(s => s.url))).toEqual(
    new Set(["https://a.com/foo", "https://b.com/bar"]),
  );
});

// --- file paths (resolver-gated) ---
const stubResolve = (p: string, line: number | null) =>
  `/file/W/${p.replace(/^\//, "")}${line ? `#L${line}` : ""}`;

test("path with slash + :line → internal file link", () => {
  const segs = computeRowLinks(["Edited apps/web/src/FolderList.tsx:142 ok"], 80, [], stubResolve);
  const f = segs.find(s => s.kind === "file");
  expect(f?.url).toBe("/file/W/apps/web/src/FolderList.tsx#L142");
  expect(f?.hint).toBe("Open apps/web/src/FolderList.tsx:142");
});

test("absolute path resolves; version string does not", () => {
  expect(computeRowLinks(["see /Users/you/x.rs here"], 80, [], stubResolve)
    .some(s => s.kind === "file")).toBe(true);
  expect(computeRowLinks(["v1.2.3 shipped"], 80, [], stubResolve)
    .some(s => s.kind === "file")).toBe(false);
});

test("bare filename links only with a :line", () => {
  expect(computeRowLinks(["see FolderList.tsx here"], 80, [], stubResolve)
    .some(s => s.kind === "file")).toBe(false);
  expect(computeRowLinks(["see FolderList.tsx:9 here"], 80, [], stubResolve)
    .some(s => s.kind === "file")).toBe(true);
});

test("bare archive filenames link without a :line", () => {
  // zip
  expect(computeRowLinks(["see backup.zip here"], 80, [], stubResolve)
    .some(s => s.kind === "file")).toBe(true);
  // tar.gz
  expect(computeRowLinks(["archive.tar.gz ready"], 80, [], stubResolve)
    .some(s => s.kind === "file")).toBe(true);
  // non-archive bare still no-link
  expect(computeRowLinks(["see readme.txt here"], 80, [], stubResolve)
    .some(s => s.kind === "file")).toBe(false);
  // .zip as part of a longer non-archive ext is NOT matched (\b gate)
  expect(computeRowLinks(["data.csv here"], 80, [], stubResolve)
    .some(s => s.kind === "file")).toBe(false);
});

test("long paths in command-block output link correctly", () => {
  // Exact text from terminal command-block rows
  const cmdRow = "│ /Users/you/Code/project/docs/report_2026-07-13.zip                                 │";
  const segs = computeRowLinks([cmdRow], 80, [], stubResolve);
  const fileLinks = segs.filter(s => s.kind === "file");
  expect(fileLinks.length).toBe(1);
  expect(fileLinks[0].url).toBe("/file/W/Users/you/Code/project/docs/report_2026-07-13.zip");
});

test("OSC-8 hyperlink on filename does not block full-path file link", () => {
  // `ls --hyperlink` emits an OSC-8 hyperlink on just the filename, but the
  // full path appears in the terminal text. The regex should still detect the
  // full path as a file link — the OSC-8 file:/// URI is useless in a browser.
  const row = "/Users/you/Code/project/docs/report_2026-07-13.zip";
  const osc8: [string, string][] = [
    ["report_2026-07-13.zip",
     "file:///Users/you/Code/project/docs/report_2026-07-13.zip"],
  ];
  const segs = computeRowLinks([row], 80, osc8, stubResolve);
  const fileLinks = segs.filter(s => s.kind === "file");
  expect(fileLinks.length).toBe(1);
  expect(fileLinks[0].url).toBe("/file/W/Users/you/Code/project/docs/report_2026-07-13.zip");
});

test("scheme URLs are NOT evicted by overlapping file-path matches", () => {
  // A path-like segment inside a URL (example.com/path/file.zip) should NOT
  // nuke the valid https:// link. Only file:// OSC-8 links are evicted.
  const row = "see https://example.com/path/file.zip here";
  const segs = computeRowLinks([row], 80, [], stubResolve);
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
  const segs = computeRowLinks(["see ~/Code/proj/a.py here"], 80, [], stubResolve);
  const fileLinks = segs.filter(s => s.kind === "file");
  expect(fileLinks.length).toBe(1);
  // The path starts with ~ (not /)
  expect(fileLinks[0].url).toContain("~/Code/proj/a.py");
});

test("~ paths are not linkified when resolver rejects them", () => {
  // Real resolveFile returns null for ~ paths (can't expand home dir client-side).
  const realResolve = (p: string, _line: number | null) =>
    p.startsWith("~") ? null : `/file/W/${p.replace(/^\//, "")}`;
  const segs = computeRowLinks(["see ~/Code/proj/a.py here"], 80, [], realResolve);
  expect(segs.filter(s => s.kind === "file").length).toBe(0);
  // A non-~ path with the same resolver still works
  const segs2 = computeRowLinks(["see /Users/you/a.py here"], 80, [], realResolve);
  expect(segs2.filter(s => s.kind === "file").length).toBe(1);
});

test("no resolver → no file links (URL-only behavior preserved)", () => {
  expect(computeRowLinks(["apps/web/foo.ts:1"], 80)).toEqual([]);
});

// --- localhost dev URLs ---
test("localhost:PORT becomes an http url; bare localhost does not", () => {
  const segs = computeRowLinks(["Local: localhost:5174/app now"], 80);
  expect(segs.find(s => s.url.startsWith("http://localhost"))?.url).toBe("http://localhost:5174/app");
  expect(computeRowLinks(["run on localhost soon"], 80).length).toBe(0);
});

// --- GitHub refs ---
test("owner/repo#123 → issues url (self-contained)", () => {
  const segs = computeRowLinks(["fixes owner/repo#412 today"], 80);
  expect(segs.find(s => s.url.includes("github.com"))?.url)
    .toBe("https://github.com/owner/repo/issues/412");
});

test("owner/repo@sha → commit url (self-contained)", () => {
  const segs = computeRowLinks(["at owner/repo@deadbeef now"], 80);
  expect(segs.find(s => s.url.includes("/commit/"))?.url)
    .toBe("https://github.com/owner/repo/commit/deadbeef");
});

test("bare #N and commit SHA link only when the session repo is known", () => {
  expect(computeRowLinks(["see #7 and a1b2c3d4 here"], 80)).toEqual([]);
  const urls = computeRowLinks(["see #7 and a1b2c3d4 here"], 80, [], undefined, "o/r").map(s => s.url);
  expect(urls).toContain("https://github.com/o/r/issues/7");
  expect(urls).toContain("https://github.com/o/r/commit/a1b2c3d4");
});

test("a pure-number token is not treated as a commit SHA", () => {
  const segs = computeRowLinks(["build 1234567 done"], 80, [], undefined, "o/r");
  expect(segs.some(s => s.url.includes("/commit/"))).toBe(false);
});

// --- OSC 8 fragment substring suppression (regression) ---

test("OSC 8 fragment that is a substring of a scheme URL does not suppress the full URL", () => {
  // `ls --hyperlink` in the repo recorded "roost" → file:///…/roost; the
  // github URL contains "roost" as a path segment and must NOT be fragmented.
  const osc8: Array<[string, string]> = [["roost", "file:///Users/mike/Code/idea/roost"]];
  const segs = computeRowLinks(["https://github.com/cefege/roost"], 80, osc8);
  expect(segs).toEqual([
    { row: 0, start: 0, end: 31, url: "https://github.com/cefege/roost" },
  ]);
});

test("short OSC 8 text (≥2 chars) inside a URL does not win over the full URL", () => {
  const osc8: Array<[string, string]> = [[".c", "file:///x/.c"]];
  const segs = computeRowLinks(["https://github.com/cefege/roost"], 80, osc8);
  expect(segs.map(s => s.url)).toEqual(["https://github.com/cefege/roost"]);
  expect(segs.map(s => s.start)).toEqual([0]);
});

test("OSC 8 link whose visible text equals the whole URL keeps the producer URI", () => {
  const osc8: Array<[string, string]> = [["https://github.com/cefege/roost", "https://github.com/cefege/roost/?custom=1"]];
  const segs = computeRowLinks(["https://github.com/cefege/roost"], 80, osc8);
  expect(segs.length).toBe(1);
  expect(segs[0].start).toBe(0);
  expect(segs[0].end).toBe(31);
  expect(segs[0].url).toBe("https://github.com/cefege/roost/?custom=1");
});

test("standalone OSC 8 file link not inside any URL still resolves to its producer URI", () => {
  const osc8: Array<[string, string]> = [["Foo.txt", "file:///tmp/Foo.txt"]];
  const segs = computeRowLinks(["see Foo.txt here"], 80, osc8);
  expect(segs.map(s => s.url)).toEqual(["file:///tmp/Foo.txt"]);
});

// ── ROW_LINK_HINT prefilter — one case per pattern family ─────────────────
// computeRowLinks skips the whole regex battery for a logical line with no hint
// character. A family that needs a character the hint does not test would go
// silently undetected, so each family is pinned here. A NEW pattern MUST arrive
// with its own case.
const resolveFile = (path: string, line: number | null): string =>
  `/file/fp/${path}${line === null ? "" : `#L${line}`}`;

test("prefilter keeps every link family detectable", () => {
  const one = (rows: string[], ownerRepo?: string): string[] =>
    computeRowLinks(rows, 80, [], resolveFile, ownerRepo).map((s) => s.url);

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
  expect(computeRowLinks(["the quick brown fox jumps over a lazy dog"], 80, [], resolveFile, "cefege/roost"))
    .toEqual([]);
});

test("prefilter never applies while an OSC 8 link is tracked", () => {
  // OSC 8 link text is arbitrary — here it has no hint character at all.
  const osc8: Array<[string, string]> = [["Readme", "file:///tmp/Readme"]];
  expect(computeRowLinks(["open Readme now"], 80, osc8).map((s) => s.url))
    .toEqual(["file:///tmp/Readme"]);
});
