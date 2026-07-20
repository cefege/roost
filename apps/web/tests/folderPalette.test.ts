import { expect, test } from "bun:test";
import { childPath, pathCrumbs, parentPath, collapseCrumbs } from "../src/lib/folderPalette.ts";

test("childPath joins, handling root + trailing slash", () => {
  expect(childPath("~/code", "roost")).toBe("~/code/roost");
  expect(childPath("~/code/", "roost")).toBe("~/code/roost");
  expect(childPath("/", "usr")).toBe("/usr");
});

test("pathCrumbs: cumulative segments, abs + home roots", () => {
  expect(pathCrumbs("/Users/you")).toEqual([
    { label: "/", path: "/" },
    { label: "Users", path: "/Users" },
    { label: "you", path: "/Users/you" },
  ]);
  expect(pathCrumbs("~/Code/roost")).toEqual([
    { label: "~", path: "~" },
    { label: "Code", path: "~/Code" },
    { label: "roost", path: "~/Code/roost" },
  ]);
  expect(pathCrumbs("")).toEqual([]);
});

test("parentPath drops last segment; roots stay put", () => {
  expect(parentPath("/Users/you")).toBe("/Users");
  expect(parentPath("~/Code")).toBe("~");
  expect(parentPath("~")).toBe("~");
  expect(parentPath("/")).toBe("/");
});

test("collapseCrumbs: ≤4 crumbs pass through unchanged", () => {
  const crumbs = pathCrumbs("/a/b/c");
  expect(collapseCrumbs(crumbs)).toEqual([
    { kind: "crumb", label: "/", path: "/" },
    { kind: "crumb", label: "a", path: "/a" },
    { kind: "crumb", label: "b", path: "/a/b" },
    { kind: "crumb", label: "c", path: "/a/b/c" },
  ]);
});

test("collapseCrumbs: ≥5 crumbs fold middle into ellipsis (head 1 + tail 2)", () => {
  const crumbs = pathCrumbs("/a/b/c/d/e");
  expect(collapseCrumbs(crumbs)).toEqual([
    { kind: "crumb", label: "/", path: "/" },
    { kind: "ellipsis", hidden: [
      { label: "a", path: "/a" },
      { label: "b", path: "/a/b" },
      { label: "c", path: "/a/b/c" },
    ] },
    { kind: "crumb", label: "d", path: "/a/b/c/d" },
    { kind: "crumb", label: "e", path: "/a/b/c/d/e" },
  ]);
});
