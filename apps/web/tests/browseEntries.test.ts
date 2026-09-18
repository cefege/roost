import { expect, test } from "bun:test";
import {
  relativeEntryTime,
  visibleFiles,
  visibleFolders,
  type BrowseEntry,
} from "../src/lib/browseEntries.ts";

const entry = (name: string, isDir: boolean, mtimeMs = 0): BrowseEntry => ({ name, isDir, mtimeMs });

const listing: BrowseEntry[] = [
  entry("zebra", true),
  entry(".hidden-dir", true),
  entry("Apple", true),
  entry("mango", true),
  entry("notes.md", false),
  entry(".env", false),
  entry("Build.log", false),
];

test("folders and files are separated, dot entries excluded from both", () => {
  expect(visibleFolders(listing, "").map((e) => e.name)).toEqual(["Apple", "mango", "zebra"]);
  expect(visibleFiles(listing, "").map((e) => e.name)).toEqual(["Build.log", "notes.md"]);
});

test("output order is alphabetical regardless of input order", () => {
  const reversed = [...listing].reverse();
  expect(visibleFolders(reversed, "").map((e) => e.name)).toEqual(["Apple", "mango", "zebra"]);
  expect(visibleFiles(reversed, "").map((e) => e.name)).toEqual(["Build.log", "notes.md"]);
});

test("filter is a case-insensitive substring match on trimmed input", () => {
  expect(visibleFolders(listing, "AN").map((e) => e.name)).toEqual(["mango"]);
  expect(visibleFolders(listing, "  ppl  ").map((e) => e.name)).toEqual(["Apple"]);
  expect(visibleFiles(listing, "LOG").map((e) => e.name)).toEqual(["Build.log"]);
  expect(visibleFolders(listing, "nope")).toEqual([]);
});

test("a whitespace-only filter matches everything, and cannot reveal dot entries", () => {
  expect(visibleFolders(listing, "   ").map((e) => e.name)).toEqual(["Apple", "mango", "zebra"]);
  expect(visibleFolders(listing, "hidden")).toEqual([]);
  expect(visibleFiles(listing, "env")).toEqual([]);
});

test("relativeEntryTime: missing mtime renders nothing", () => {
  const now = 1_700_000_000_000;
  expect(relativeEntryTime(0, now)).toBe("");
  expect(relativeEntryTime(-1, now)).toBe("");
});

test("relativeEntryTime crosses each unit boundary on the injected clock", () => {
  const now = 1_700_000_000_000;
  expect(relativeEntryTime(now, now)).toBe("just now");
  expect(relativeEntryTime(now - 59_999, now)).toBe("just now");
  expect(relativeEntryTime(now - 60_000, now)).toBe("1m ago");
  expect(relativeEntryTime(now - 3_599_999, now)).toBe("59m ago");
  expect(relativeEntryTime(now - 3_600_000, now)).toBe("1h ago");
  expect(relativeEntryTime(now - 86_399_999, now)).toBe("23h ago");
  expect(relativeEntryTime(now - 86_400_000, now)).toBe("1d ago");
  expect(relativeEntryTime(now - 604_799_999, now)).toBe("6d ago");
});

test("relativeEntryTime falls back to a calendar date beyond a week", () => {
  const now = 1_700_000_000_000;
  const old = now - 604_800_000;
  expect(relativeEntryTime(old, now)).toBe(
    new Date(old).toLocaleDateString(undefined, { month: "short", day: "numeric" }),
  );
  expect(relativeEntryTime(old, now)).not.toContain("ago");
});
