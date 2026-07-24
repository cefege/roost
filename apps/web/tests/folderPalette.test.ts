import { expect, test } from "bun:test";
import { childPath, pathCrumbs, parentPath, collapseCrumbsTo } from "../src/lib/folderPalette.ts";

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

test("collapseCrumbsTo: hideMiddle 0 returns all crumbs, no ellipsis", () => {
  const crumbs = pathCrumbs("/a/b/c/d/e");
  expect(collapseCrumbsTo(crumbs, 0)).toEqual([
    { kind: "crumb", label: "/", path: "/" },
    { kind: "crumb", label: "a", path: "/a" },
    { kind: "crumb", label: "b", path: "/a/b" },
    { kind: "crumb", label: "c", path: "/a/b/c" },
    { kind: "crumb", label: "d", path: "/a/b/c/d" },
    { kind: "crumb", label: "e", path: "/a/b/c/d/e" },
  ]);
});

test("collapseCrumbsTo: folds from the left of the middle, keeps parent+current", () => {
  const crumbs = pathCrumbs("/a/b/c/d/e"); // middle = [a,b,c]; parent=d, current=e
  expect(collapseCrumbsTo(crumbs, 2)).toEqual([
    { kind: "crumb", label: "/", path: "/" },
    { kind: "ellipsis", hidden: [
      { label: "a", path: "/a" },
      { label: "b", path: "/a/b" },
    ] },
    { kind: "crumb", label: "c", path: "/a/b/c" },
    { kind: "crumb", label: "d", path: "/a/b/c/d" },
    { kind: "crumb", label: "e", path: "/a/b/c/d/e" },
  ]);
});

test("collapseCrumbsTo: max collapse = root + ellipsis(all middle) + parent + current", () => {
  const crumbs = pathCrumbs("/a/b/c/d/e");
  expect(collapseCrumbsTo(crumbs, 3)).toEqual([
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

test("collapseCrumbsTo: <=3 crumbs never collapse; hideMiddle clamped", () => {
  expect(collapseCrumbsTo(pathCrumbs("/a/b"), 5)).toEqual([
    { kind: "crumb", label: "/", path: "/" },
    { kind: "crumb", label: "a", path: "/a" },
    { kind: "crumb", label: "b", path: "/a/b" },
  ]);
  expect(collapseCrumbsTo(pathCrumbs("/a/b/c/d/e"), 99)).toEqual(
    collapseCrumbsTo(pathCrumbs("/a/b/c/d/e"), 3),
  );
});
